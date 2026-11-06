/// 稳定mesh身份与全局槽共同校验，删除/重建只局部拒绝旧路径，不宣布全局失效。
module restir_scene;
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

[[vk::binding(37, 0)]]
StructuredBuffer<uint4> restir_positions;

/// 头部后的quad数据与容量尾部的活动域目录在同一事务发布。
uint4 restir_domain(uint token)
{
    uint base = restir_positions[0].y;
    uint index = geometry::identity_index(restir_positions, base, token);
    return index == 0xffffffffu ? uint4(0) : restir_positions[base + 1u + index];
}
public uint restir_primitive(restir_pt::HitInfo hit)
{
    if (!hit.isValid())
        return 0xffffffffu;
    uint4 domain = restir_domain(hit.data.x);
    return domain.w == hit.data.x && (hit.data.y >> 1u) < domain.y ? domain.x * 2u + hit.data.y
                                                                   : 0xffffffffu;
}
public bool restir_valid_hit(restir_pt::HitInfo hit)
{
    return restir_primitive(hit) != 0xffffffffu;
}
public restir_pt::HitInfo restir_stable_hit(uint primitive, float2 bary)
{
    uint token = restir_positions[1u + (primitive >> 1u) * 4u + 3u].w;
    uint4 domain = restir_domain(token);
    if (token == 0u || domain.w != token)
        return {};
    return { uint4(token, primitive - domain.x * 2u, asuint(bary)) };
}

uint restir_normal(uint primitive, float2 bary, float3 normal)
{
    if ((parameters[0].output_size.z & 4u) == 0)
        return 0;
    uint start = 1u + (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(asfloat(restir_positions[start]),
                                                             asfloat(restir_positions[start + 1u]),
                                                             asfloat(restir_positions[start + 2u]));
    bool second = (primitive & 1u) != 0;
    return mapped_shading_normal(primitive, bary, (second ? vertices.c : vertices.b) - vertices.a,
                                 (second ? vertices.d : vertices.c) - vertices.a,
                                 float3x3(1, 0, 0, 0, 1, 0, 0, 0, 1), normal);
}

public geometry::TriangleEndpoint restir_endpoint(uint primitive, float2 bary)
{
    uint start = 1u + (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(asfloat(restir_positions[start]),
                                                             asfloat(restir_positions[start + 1u]),
                                                             asfloat(restir_positions[start + 2u]));
    pt::QuadAreaSample sample = { primitive & 1u, bary, 0 };
    float3 translation =
        float3(asint(restir_positions[start + 3u].xyz) - parameters[0].anchor_region.xyz) * 64.0;
    float3x4 transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    float3x4 inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    return pt::quad_endpoint(vertices, sample, transform, inverse);
}

public geometry::TriangleEndpoint restir_endpoint(restir_pt::HitInfo identity)
{
    return restir_endpoint(restir_primitive(identity), asfloat(identity.data.zw));
}

/// 准入读取源能力，不合并两种宿主的材质解码。MERS含未知SSS时保守放宽候选。
public restir_pt::AdmissionSurface reuse_geometry(restir_pt::HitInfo hit,
                                                  geometry::SurfacePoint point)
{
    ModMaterial material = materials[material_id(restir_primitive(hit))];
    bool bothSides = (material.control.z & (64u | 1u | 256u)) != 0 ||
                     (material.control.x & 2u) != 0 || material.surface.z > 0.0;
    return restir_pt::admission_surface(point, parameters[0].origin.xyz, bothSides);
}

public struct RestirScene : restir_pt::IRestirScene
{
    public restir_pt::Surface loadSurface(restir_pt::HitInfo identity, float3 wo, bool previous)
    {
        uint primitive = restir_primitive(identity);
        if (primitive == 0xffffffffu)
            return {};
        geometry::SurfacePoint point =
            geometry::surface_point(restir_endpoint(primitive, asfloat(identity.data.zw)));
        return surface_from_resolved(identity, primitive, point, wo);
    }
    restir_pt::Surface surface_from_resolved(restir_pt::HitInfo identity, uint primitive,
                                             geometry::SurfacePoint point, float3 wo)
    {
        pt::Hit source = {};
        source.surface = point;
        source.identity = uint4(primitive, identity.data.zw,
                                restir_normal(primitive, asfloat(identity.data.zw), point.normal));
        return surface_from_hit(identity, source, wo);
    }
    public restir_pt::Surface surface_from_hit(restir_pt::HitInfo identity, pt::Hit sourceHit,
                                               float3 wo)
    {
        restir_pt::Surface result = {};
        uint primitive = sourceHit.identity.x;
        result.point = sourceHit.surface;
        result.hit = identity;
        result.wo = wo;
        result.frontFacing = dot(wo, result.point.normal) >= 0;
        result.identity = sourceHit.identity;
        pt::Hit hit = restir_hit(result);
        // Surface只缓存算法需要的元数据；albedo由Flow叶子按需读取。
        // 这里只消费roughness/IOR/metalness/发光元数据；非发光表面不读取albedo/tint。
        ModMaterial source = materials[material_id(primitive)];
        uint side = result.frontFacing ? 1u : 2u;
        if (source.emission.w > 0 && (source.control.y & side) != 0 &&
            ((source.control.z & 32u) == 0 || result.frontFacing))
            hit.texel = material_texel(primitive, asfloat(identity.data.zw));
        pt::SurfaceMaterial material =
            resolve_material(hit, ~0u, ~0u, parameters[0].block_emission_scale);
        result.emission = material.emission;
        result.transmitted = material.transmitted;
        result.incident =
            result.frontFacing || (material.pbr.control.x & 3u) != 1u
                ? float4(1, 0, 0, 0)
                : float4(material.pbr.surface.x, materials[material_id(primitive)].extinction.yzw);
        result.capability = material.pbr.control.x;
        result.roughness = material.pbr.base_roughness.w;
        // 通用算法不解释material；此宿主缓存metalness供setIncident作cheap lobe判断。
        result.material = asuint(material.pbr.surface.y);
        result.supportSimpleRoughnessThreshold = false;
        result.hasNonDeltaLobes =
            !openpbr::is_discrete_only(material.pbr, result.incident, material.transmitted);
        return result;
    }
    public restir_pt::Surface loadSurfaceFromOrigin(restir_pt::HitInfo hit, float3 origin,
                                                    bool previous)
    {
        uint primitive = restir_primitive(hit);
        if (primitive == 0xffffffffu)
            return {};
        geometry::SurfacePoint point =
            geometry::surface_point(restir_endpoint(primitive, asfloat(hit.data.zw)));
        float3 delta = origin - point.position;
        float distance = length(delta);
        restir_pt::Surface result = surface_from_resolved(hit, primitive, point, delta / distance);
        result.distance = distance;
        return result;
    }
    public restir_pt::Surface loadSurfaceWithDistance(restir_pt::HitInfo hit, float3 wo,
                                                      bool previous, float distance)
    {
        restir_pt::Surface result = loadSurface(hit, wo, previous);
        result.distance = distance;
        return result;
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
        if (!geometry::connection_ray(
                source.point, restir_endpoint(target.identity.x, asfloat(target.hit.data.zw)), ray))
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
        uint program = materials[material_id(surface.identity.x)].control.x >> 2u;
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
        uint program = (materials[material_id(surface.identity.x)].control.x >> 2u) +
                       (continuation ? 0u : asuint(parameters[0].restir.z));
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        if (continuation)
        {
            RestirInitialCall call = {
                material_hit(surface), surface.incident, surface.wo, u, direction,
                secondDirection
            };
            CallShader(program, call);
            return call.result();
        }
        RestirDirectCall call = { material_hit(surface), surface.wo, direction, surface.incident.x,
                                  secondDirection };
        CallShader(program, call);
        return { {},
                 { asfloat(call.hit.identity.yzw), asfloat(call.hit.identity.x) },
                 call.hit.normal,
                 { call.wo, call.direction.x } };
    }
    public restir_pt::BsdfEvaluation evaluateBsdf(restir_pt::Surface surface, float3 wi,
                                                  int component)
    {
        uint program = (materials[material_id(surface.identity.x)].control.x >> 2u) +
                       asuint(parameters[0].restir.z);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        RestirEvaluateCall call = { material_hit(surface), surface.wo, wi, surface.incident.x };
        CallShader(program, call);
        return { asfloat(call.hit.identity.yzw), asfloat(call.hit.identity.x) };
    }
    public bool isSampleRough(restir_pt::Surface surface, uint component, float pdf,
                              float threshold)
    {
        return pdf <= 1.0 / (threshold * threshold);
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
            pt::Hit hit = {};
            hit.identity = uint4(primitive, light.data.zw, 0);
            hit.surface =
                geometry::surface_point(restir_endpoint(primitive, asfloat(light.data.zw)));
            return scene.emission_pdf(hit, source.point.position);
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
