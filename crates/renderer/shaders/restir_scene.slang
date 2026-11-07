/// 稳定mesh身份与全局槽共同校验，删除/重建只局部拒绝旧路径，不宣布全局失效。
module restir_scene;
__exported import restir_geometry;
import restir_call;
import mod_scene;
import mod_lights;
import params;
import bsdf.openpbr.api;
import pt.scene;
import geometry.ray;
import geometry.identity;
import restir_pt.scene;
import restir_pt.types;
import restir_pt.admission;
import geometry.camera;
import pt.light_sampling;
import atmosphere.parameters;
import atmosphere.sampling;

/// 准入读取源能力，不合并两种宿主的材质解码。MERS含未知SSS时保守放宽候选。
public restir_pt::AdmissionSurface reuse_geometry(restir_pt::HitInfo hit,
                                                  geometry::SurfacePoint point)
{
    ModMaterial material = material_at(material_id(restir_primitive(hit)));
    bool bothSides = (material.control.z & (64u | 1u | 256u)) != 0 ||
                     (material.control.x & 2u) != 0 || material.surface.z > 0.0;
    return restir_pt::admission_surface(point, parameters[0].origin.xyz, bothSides);
}

public struct RestirScene : restir_pt::IRestirScene
{
    /// 只恢复有效几何；compute准入/探针不能隐式执行材料callable。
    public restir_pt::Surface loadGeometrySurface(restir_pt::HitInfo identity, float3 wo)
    {
        uint primitive = restir_primitive(identity);
        if (primitive == 0xffffffffu)
            return {};
        restir_pt::Surface result = {};
        result.hit = identity;
        result.point =
            geometry::surface_point(restir_endpoint(primitive, asfloat(identity.data.zw)));
        result.identity = uint4(primitive, identity.data.zw, 0);
        result.wo = wo;
        result.frontFacing = dot(wo, result.point.normal) >= 0;
        return result;
    }
    public restir_pt::Surface loadGeometrySurfaceFromOrigin(restir_pt::HitInfo identity,
                                                            float3 origin)
    {
        restir_pt::Surface result = loadGeometrySurface(identity, float3(0));
        if (!result.hit.isValid())
            return result;
        float3 delta = origin - result.point.position;
        result.distance = length(delta);
        result.wo = delta / result.distance;
        result.frontFacing = dot(result.wo, result.point.normal) >= 0;
        return result;
    }
    restir_pt::Surface surface_metadata(restir_pt::Surface result)
    {
        if (!result.hit.isValid())
            return result;
        uint program = metadata_program(result.identity.x);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        RestirMetadataCall call = RestirMetadataCall::input(result.identity, result.frontFacing);
        CallShader(program, call);
        call.apply(result);
        result.supportSimpleRoughnessThreshold = false;
        return result;
    }
    public restir_pt::Surface loadSurface(restir_pt::HitInfo identity, float3 wo, bool previous)
    {
        return surface_metadata(loadGeometrySurface(identity, wo));
    }
    public restir_pt::Surface geometry_surface_from_hit(restir_pt::HitInfo identity,
                                                        pt::Hit sourceHit, float3 wo)
    {
        restir_pt::Surface result = {};
        result.point = sourceHit.surface;
        result.hit = identity;
        result.wo = wo;
        result.frontFacing = dot(wo, result.point.normal) >= 0;
        result.identity = uint4(sourceHit.identity.xyz, 0);
        return result;
    }
    public restir_pt::Surface loadSurfaceFromOrigin(restir_pt::HitInfo hit, float3 origin,
                                                    bool previous)
    {
        return surface_metadata(loadGeometrySurfaceFromOrigin(hit, origin));
    }
    public restir_pt::Surface loadSurfaceWithDistance(restir_pt::HitInfo hit, float3 wo,
                                                      bool previous, float distance)
    {
        restir_pt::Surface result = loadSurface(hit, wo, previous);
        result.distance = distance;
        return result;
    }
    public geometry::SurfacePoint lightEndpoint(restir_pt::HitInfo hit, bool previous)
    {
        return geometry::surface_point(restir_endpoint(hit));
    }
    public void setIncident(inout restir_pt::Surface surface, float4 medium)
    {
        surface.incident = medium;
        openpbr::Material material = {};
        material.control.x = surface.capability;
        material.base_roughness.w = surface.roughness;
        material.surface.y = asfloat(surface.material);
        surface.hasNonDeltaLobes =
            !openpbr::is_discrete_only(material, medium, surface.transmitted);
    }
    public bool trace(float3 origin, float3 direction, bool previous, out restir_pt::HitInfo hit,
                      out float distance)
    {
        TracePayload found = {};
        found.primitive = 0xffffffffu;
        hit = {};
        distance = pt::FAR;
        RayDesc ray = { origin, 0, direction, pt::FAR };
        TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, found);
        if (found.primitive == 0xffffffffu)
            return false;
        hit = restir_stable_hit(found.primitive, found.bary);
        distance = found.distance;
        return true;
    }
    public bool visible(restir_pt::Surface source, restir_pt::Surface target, bool previous)
    {
        geometry::Ray ray;
        if (!geometry::connection_ray(source.point, restir_endpoint(target.hit), ray))
            return false;
        ModScene scene;
        return !scene.occluded(ray);
    }
    public bool visibleDirection(restir_pt::Surface source, float3 direction, bool previous)
    {
        ModScene scene;
        geometry::Ray ray = { geometry::offset_origin(source.point, direction), 0, direction,
                              pt::FAR };
        return !scene.occluded(ray);
    }
    public float3 transmittance(restir_pt::Surface source, float3 direction, float distance)
    {
        bool crossing =
            dot(source.point.normal, direction) * dot(source.point.normal, source.wo) < 0;
        float4 medium =
            crossing && (source.capability & 3u) == 1u ? source.transmitted : source.incident;
        return exp(-medium.yzw * distance);
    }
    public float environmentDistance()
    {
        return pt::FAR;
    }
    public restir_pt::BsdfSample sampleBsdf(restir_pt::Surface surface, float3 u)
    {
        uint program = material_program(surface.identity.x, MATERIAL_REPLAY);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        RestirReplayCall call = { material_hit(surface), surface.incident, surface.wo, u };
        CallShader(program, call);
        return sampled_result(call.hit, call.incident);
    }
    public restir_pt::InitialShading shadeInitial(restir_pt::Surface surface, float3 u,
                                                  float3 direction, float3 secondDirection,
                                                  bool continuation)
    {
        // 兼容解析测试的旧接口；生产初采只调用完整的shadeInitialVertex。
        restir_pt::BsdfSample sampled = {};
        if (continuation)
            sampled = sampleBsdf(surface, u);
        float3 emission =
            material_emission(surface.identity.x, asfloat(surface.identity.yz), surface.frontFacing,
                              parameters[0].block_emission_scale);
        return { sampled, evaluateBsdf(surface, direction, 0), emission,
                 evaluateBsdf(surface, secondDirection, 0) };
    }
    public restir_pt::InitialVertex shadeInitialVertex(restir_pt::Surface surface,
                                                       restir_pt::InitialRequest request)
    {
        bool continuation = (request.flags & restir_pt::INITIAL_SAMPLE) != 0;
        uint program = material_program(surface.identity.x,
                                        continuation ? MATERIAL_INITIAL : MATERIAL_INITIAL_DIRECT);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        if (continuation)
        {
            RestirInitialCall call = RestirInitialCall::input(surface, request, surface.identity);
            CallShader(program, call);
            return call.result();
        }
        RestirInitialDirectCall direct =
            RestirInitialDirectCall::input(surface, request, surface.identity);
        CallShader(program, direct);
        return direct.result();
    }
    public restir_pt::BsdfEvaluation evaluateBsdf(restir_pt::Surface surface, float3 wi,
                                                  int component)
    {
        uint program = material_program(surface.identity.x, MATERIAL_EVALUATE);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        RestirEvaluateCall call = { material_hit(surface), surface.wo, wi, surface.incident.x };
        CallShader(program, call);
        return { asfloat(call.hit.identity.yzw), asfloat(call.hit.identity.x) };
    }
    public restir_pt::ConnectionEvaluation evaluateConnection(restir_pt::Surface surface, float3 wi,
                                                              float distance, int component)
    {
        uint program = material_program(surface.identity.x, MATERIAL_CONNECTION);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        RestirConnectionCall call = { material_hit(surface), surface.wo, wi, surface.incident,
                                      distance };
        CallShader(program, call);
        return { { asfloat(call.hit.identity.yzw), asfloat(call.hit.identity.x) },
                 call.hit.normal };
    }
    public bool isSampleRough(restir_pt::Surface surface, uint component, float pdf,
                              float threshold)
    {
        return pdf <= 1.0 / (threshold * threshold);
    }
    public restir_pt::EmitterEvaluation evaluateEmitter(restir_pt::Surface surface, float3 from,
                                                        bool previous)
    {
        uint primitive = restir_primitive(surface.hit);
        uint program = material_program(primitive, MATERIAL_EMITTER);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        RestirEmitterCall call = RestirEmitterCall::input(surface.hit, from, surface.wo);
        CallShader(program, call);
        return { call.emission, call.pdf, call.position, call.normal };
    }
    public float environmentPdf(float3 direction)
    {
        if (parameters[0].lighting.w <= 0)
            return 0;
        float cosine = cos(parameters[0].atmosphere.sun.z);
        float sun = dot(direction, parameters[0].lighting.xyz) >= cosine
                        ? 1.0 / (2.0 * atmosphere::PI * (1.0 - cosine))
                        : 0;
        return sun;
    }
    public restir_pt::LightSample sampleLight(restir_pt::Surface surface, float3 u)
    {
        ModScene scene;
        bool sky = parameters[0].lighting.w > 0;
        restir_pt::LightSample result = {};
        // 初采固定两族各取一个样本；返回条件密度，不含二选一的族概率。
        if (pt::local_light_family(u))
        {
            pt::LightSample quad = scene.sample_light(surface.point, u);
            if (quad.pdf <= 0)
                return result;
            result.hit = restir_stable_hit(light_meta[quad.emission_reference].w * 2u +
                                               (asuint(quad.emission.z) & 1u),
                                           quad.emission.xy);
            result.direction = quad.direction;
            result.distance = quad.distance;
            result.pdf = quad.pdf;
            result.radiance = scene.light_emission(surface.point, quad, false);
            result.lightType = restir_pt::LIGHT_EMISSIVE;
        }
        else if (sky)
        {
            float3 direction = scene.sample_sun(surface.point, u.xy).direction;
            // 与BSDF环境端点采用相同的总辐射域，太阳盘内的天空项也须配对MIS。
            pt::Environment radiance = scene.environment(direction, surface.point.position, 1);
            result.direction = direction;
            result.distance = pt::FAR;
            result.pdf = environmentPdf(direction);
            result.radiance = radiance.sky + radiance.sun;
            result.lightType = restir_pt::LIGHT_ENV;
        }
        return result;
    }
    public float lightPdf(restir_pt::Surface source, restir_pt::HitInfo light, float3 direction,
                          uint type, bool previous)
    {
        ModScene scene;
        if (type == restir_pt::LIGHT_EMISSIVE)
        {
            uint primitive = restir_primitive(light);
            if (primitive == 0xffffffffu)
                return 0;
            uint program = material_program(primitive, MATERIAL_PDF);
#ifdef PRIMEPT_SER
            ReorderThread(program, 32u);
#endif
            RestirPdfCall call = { light.data, source.point.position };
            CallShader(program, call);
            return asfloat(call.identity.x);
        }
        return environmentPdf(direction);
    }
    public float3 environment(float3 direction, float3 origin, bool previous)
    {
        ModScene scene;
        pt::Environment environment = scene.environment(direction, origin, 1);
        return environment.sky + environment.sun;
    }
    public bool isPointLight(restir_pt::HitInfo light)
    {
        return false;
    }
    public float3 initialOrigin(uint2 pixel)
    {
        return parameters[0].origin.xyz;
    }
    public float3 initialDirection(uint2 pixel)
    {
        Params p = parameters[0];
        return geometry::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz,
                                                 (float2(pixel) + 0.5 + p.reconstruction.xy) /
                                                     float2(p.image.xy));
    }
    public float4 initialMedium(uint2 pixel)
    {
        return parameters[0].initial_medium;
    }
    public float primaryRayFootprint(restir_pt::Surface surface, uint2 pixel, bool previous)
    {
        return (surface.distance * surface.distance) /
               abs((1.0 / (4.0 * atmosphere::PI)) * dot(surface.wo, surface.point.normal));
    }
}
