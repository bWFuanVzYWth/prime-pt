/// 两种实时积分器共用的真实表面/反射代理导引；不维护独立历史。
module mod_guides;
import mod_scene;
import params;
import pt.denoise;
import pt.integrator;
import pt.reconstruction;
import pt.key_guides;
import pt.hit_transport;
import restir_pt.types;
import restir_geometry;
import geometry.camera;

[[vk::binding(19, 0)]]
[[vk::image_format("r32f")]]
RWTexture2D<float> guide_depth;
[[vk::binding(20, 0)]]
[[vk::image_format("rg16f")]]
RWTexture2D<float2> guide_motion;
[[vk::binding(21, 0)]]
[[vk::image_format("rgba16f")]]
RWTexture2D<float4> guide_normal;
[[vk::binding(22, 0)]]
[[vk::image_format("rgba16f")]]
RWTexture2D<float4> guide_diffuse;
[[vk::binding(23, 0)]]
[[vk::image_format("rgba16f")]]
RWTexture2D<float4> guide_specular;
[[vk::binding(24, 0)]]
[[vk::image_format("r32f")]]
RWTexture2D<float> guide_distance;
[[vk::binding(25, 0)]]
[[vk::image_format("rg16f")]]
RWTexture2D<float2> guide_specular_motion;

pt::KeyGuideVertex known_guide_vertex(restir_pt::HitInfo identity, float3 incoming, float4 incident)
{
    uint primitive = restir_primitive(identity);
    uint program = guide_program(primitive, true, true);
    pt::KeyGuideCall call = {};
    call.value.point.position = asfloat(identity.data.xyz);
    call.value.distance = asfloat(identity.data.w);
    call.value.canonicalDirection = incoming;
    call.value.medium = incident;
#ifdef PRIMEPT_SER
    ReorderThread(program, 32u);
#endif
    CallShader(program, call);
    return call.value;
}

/// 无SER能力的ReSTIR仍按known-surface键处理辅助材料，不引入HitObject能力。
public struct RestirGuideTrace : pt::IKeyGuideTrace
{
    public bool intersect_guide(float3 origin, float3 direction, float4 incident,
                                out pt::KeyGuideVertex value)
    {
        value = {};
        pt::Hit hit;
        if (!ModScene().intersect(origin, direction, hit))
            return false;
        restir_pt::HitInfo identity = restir_stable_hit(hit.identity.x, asfloat(hit.identity.yz));
        value = known_guide_vertex(identity, direction, incident);
        value.distance = hit.distance;
        return true;
    }
};

public struct GuideTrace : pt::IKeyGuideTrace
{
    public bool restir;

    public bool intersect_guide(float3 origin, float3 direction, float4 incident,
                                out pt::KeyGuideVertex value)
    {
        value = {};
        RayDesc ray = { origin, 0, direction, pt::FAR };
        uint unused = 0;
        HitObject found = HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
        if (!found.IsHit())
            return false;
        uint primitive = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
#ifdef PRIMEPT_SEPARATE_GUIDES
        uint program = guide_slot(primitive);
#else
        uint program = guide_program(primitive, false, restir);
#endif
        found.SetShaderTableIndex(program + 1u);
        pt::KeyGuideCall call = {};
        call.value.medium = incident;
#ifdef PRIMEPT_SER
        ReorderThread(found);
#endif
        HitObject::Invoke(found, call);
        value = call.value;
        return true;
    }
};

public struct Guides : pt::IPrefixGuides, pt::IHitGuides
{
    public uint2 coordinate;
    public float2 film;
    public pt::DenoiseCamera current;
    public pt::DenoiseCamera previous;

    public void write_key<Trace : pt::IKeyGuideTrace>(Trace provider, pt::KeyGuideVertex first,
                                                      float3 origin, float3 direction,
                                                      float4 incident, bool deltaOnly = false)
    {
        Params p = parameters[0];
        pt::KeyGuideEndpoint endpoint = pt::trace_key_guide(
            provider, first, origin, direction, incident, p.options.y, false, deltaOnly);
        pt::GuideProjection projection = pt::project_guide(
            endpoint.endpoint, current, previous, film, p.image.xy, p.reconstruction.z > 0);
        pt::DenoiseGuides albedos = endpoint.albedos;
        guide_depth[coordinate] = projection.depth;
        guide_motion[coordinate] = projection.motion;
        guide_normal[coordinate] = float4(projection.normal, albedos.normal_roughness.w);
        guide_diffuse[coordinate] = float4(albedos.diffuse_albedo, 0);
        guide_specular[coordinate] = float4(albedos.specular_albedo, 0);

        pt::KeyGuideEndpoint reflection = pt::trace_key_guide(
            provider, first, origin, direction, incident, p.options.y, true, deltaOnly);
        pt::GuideProjection reflected = pt::project_guide(reflection.endpoint, current, previous,
                                                          film, p.image.xy, p.reconstruction.z > 0);
        guide_distance[coordinate] =
            (reflection.endpoint.flags & pt::GUIDE_SKY) != 0 ? 0 : reflection.endpoint.distance;
        guide_specular_motion[coordinate] =
            (reflected.flags & pt::GUIDE_INVALID) != 0 ? projection.motion : reflected.motion;
    }

    public void surface_hit(HitObject found, pt::TransportCall data)
    {
        uint primitive = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
        uint program = guide_program(primitive, false, false);
        found.SetShaderTableIndex(program + 1u);
        pt::KeyGuideCall call = {};
        call.value.medium = data.path.medium;
#ifdef PRIMEPT_SER
        ReorderThread(found);
#endif
        HitObject::Invoke(found, call);
        GuideTrace provider = { false };
        write_key(provider, call.value, data.path.origin, data.path.direction, data.path.medium);
    }

    public void surface_known(restir_pt::HitInfo identity, pt::TransportCall data)
    {
        pt::KeyGuideVertex first =
            known_guide_vertex(identity, data.path.direction, data.path.medium);
// 原生初采的兼容变体只用known-surface Guide；完整HG的Guide由独立管线调度。
#if defined(PRIMEPT_SER) && !defined(PRIMEPT_INITIAL_HIT)
        GuideTrace provider = { true };
        write_key(provider, first, data.path.origin, data.path.direction, data.path.medium);
#else
        write_key(RestirGuideTrace(), first, data.path.origin, data.path.direction,
                  data.path.medium);
#endif
    }

    // mode0参考与未启用EXT的兼容PT保留旧导引；HG/PT和ReSTIR不经此通用翻译路径。
    public void surface(pt::TransportCall data)
    {
        ModScene provider;
        Params p = parameters[0];
        pt::GuideEndpoint endpoint = pt::trace_guide(provider, data, p.options.y);
        pt::GuideProjection projection = pt::project_guide(endpoint, current, previous, film,
                                                           p.image.xy, p.reconstruction.z > 0);
        pt::DenoiseGuides guides = pt::guide_albedos(provider.energy(), endpoint);
        guide_depth[coordinate] = projection.depth;
        guide_motion[coordinate] = projection.motion;
        guide_normal[coordinate] = float4(projection.normal, guides.normal_roughness.w);
        guide_diffuse[coordinate] = float4(guides.diffuse_albedo, 0);
        guide_specular[coordinate] = float4(guides.specular_albedo, 0);

        pt::GuideEndpoint reflection = pt::trace_reflection_guide(provider, data, p.options.y);
        pt::GuideProjection reflected = pt::project_guide(reflection, current, previous, film,
                                                          p.image.xy, p.reconstruction.z > 0);
        guide_distance[coordinate] =
            (reflection.flags & pt::GUIDE_SKY) != 0 ? 0 : reflection.distance;
        guide_specular_motion[coordinate] =
            (reflected.flags & pt::GUIDE_INVALID) != 0 ? projection.motion : reflected.motion;
    }

    public void environment(pt::TransportCall data)
    {
        sky(data.path.direction);
    }

    public void sky(float3 direction)
    {
        guide_depth[coordinate] = pt::DENOISE_SKY_DEPTH;
        float2 pixels = 0;
        if (parameters[0].reconstruction.z > 0 &&
            (!pt::denoise_motion(previous, film, direction, true, parameters[0].image.xy, pixels) ||
             any(abs(pixels) > 65504)))
            pixels = 0;
        guide_motion[coordinate] = pixels;
        guide_normal[coordinate] = float4(0, 0, 0, 1);
        guide_diffuse[coordinate] = float4(0.5, 0.5, 0.5, 0);
        guide_specular[coordinate] = float4(0.5, 0.5, 0.5, 0);
        guide_distance[coordinate] = 0;
        guide_specular_motion[coordinate] = pixels;
    }
};
