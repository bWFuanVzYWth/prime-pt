/// 实时 PT 的真实表面/反射代理导引；不维护独立历史。
module mod_guides;
import mod_scene;
import params;
import pt.denoise;
import pt.integrator;
import pt.reconstruction;
import pt.key_guides;
import pt.guide_output;
import pt.hit_transport;
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

public struct GuideTrace : pt::IKeyGuideTrace
{

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
        uint program = guide_slot(primitive) + parameters[0].programs.z;
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

    pt::GuideImages images()
    {
        return { guide_depth,    guide_motion,   guide_normal,         guide_diffuse,
                 guide_specular, guide_distance, guide_specular_motion };
    }
    pt::GuideFrame frame()
    {
        Params p = parameters[0];
        return { current, previous, p.image.xy, p.options.y, p.reconstruction.z > 0 };
    }

    public void write_key<Trace : pt::IKeyGuideTrace>(Trace provider, pt::KeyGuideVertex first,
                                                      float3 origin, float3 direction,
                                                      float4 incident, bool deltaOnly = false)
    {
        pt::write_key_guides(provider, images(), frame(), coordinate, film, first, origin,
                             direction, incident, deltaOnly);
    }

    public void surface_hit(HitObject found, pt::TransportCall data)
    {
        uint primitive = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
        uint program = guide_slot(primitive) + parameters[0].programs.z;
        found.SetShaderTableIndex(program + 1u);
        pt::KeyGuideCall call = {};
        call.value.medium = data.path.medium;
#ifdef PRIMEPT_SER
        ReorderThread(found);
#endif
        HitObject::Invoke(found, call);
        GuideTrace provider;
        write_key(provider, call.value, data.path.origin, data.path.direction, data.path.medium);
    }

    // mode0参考与未启用EXT的兼容PT保留旧导引；HG/PT不经此通用翻译路径。
    public void surface(pt::TransportCall data)
    {
        pt::write_surface_guides(ModScene(), images(), frame(), coordinate, film, data);
    }

    public void environment(pt::TransportCall data)
    {
        sky(data.path.direction);
    }

    public void sky(float3 direction)
    {
        pt::write_sky_guide(images(), frame(), coordinate, film, direction, 0);
    }
};
