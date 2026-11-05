/// 稳定mesh身份与全局槽共同校验，删除/重建只局部拒绝旧路径，不宣布全局失效。
module restir_scene;
import restir_call;
import mod_scene;
import mod_lights;
import params;
import bsdf.openpbr.api;
import pt.scene;
import geometry.ray;
import restir_pt.scene;
import restir_pt.types;
import geometry.camera;
import pt.light_sampling;
import atmosphere.parameters;
import atmosphere.sampling;

[[vk::binding(37, 0)]]
StructuredBuffer<float4> restir_positions;

public bool restir_valid_hit(restir_pt::HitInfo hit)
{
    if (!hit.isValid())
        return false;
    uint count, stride;
    restir_positions.GetDimensions(count, stride);
    uint slot = (hit.data.x - 1u) >> 1u;
    if (slot >= count / 4u || hit.data.w == 0)
        return false;
    return asuint(restir_positions[slot * 4u + 3u].w) == hit.data.w;
}

public restir_pt::HitInfo restir_stable_hit(uint primitive, float2 bary)
{
    return { uint4(primitive + 1u, asuint(bary),
                   asuint(restir_positions[(primitive >> 1u) * 4u + 3u].w)) };
}

uint restir_normal(restir_pt::HitInfo hit, float3 normal)
{
    if ((parameters[0].output_size.z & 4u) == 0)
        return 0;
    uint primitive = hit.data.x - 1u;
    uint start = (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(
        restir_positions[start], restir_positions[start + 1u], restir_positions[start + 2u]);
    bool second = (primitive & 1u) != 0;
    return mapped_shading_normal(primitive, asfloat(hit.data.yz),
                                 (second ? vertices.c : vertices.b) - vertices.a,
                                 (second ? vertices.d : vertices.c) - vertices.a,
                                 float3x3(1, 0, 0, 0, 1, 0, 0, 0, 1), normal);
}

public geometry::TriangleEndpoint restir_endpoint(restir_pt::HitInfo identity)
{
    uint primitive = identity.data.x - 1u;
    uint start = (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(
        restir_positions[start], restir_positions[start + 1u], restir_positions[start + 2u]);
    pt::QuadAreaSample sample = { primitive & 1u, asfloat(identity.data.yz), 0 };
    float3 translation =
        float3(asint(restir_positions[start + 3u].xyz) - parameters[0].anchor_region.xyz) * 64.0;
    float3x4 transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    float3x4 inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    return pt::quad_endpoint(vertices, sample, transform, inverse);
}

public struct RestirScene : restir_pt::IRestirScene
{
    public restir_pt::Surface loadSurface(restir_pt::HitInfo identity, float3 wo, bool previous)
    {
        restir_pt::Surface result = {};
        if (!restir_valid_hit(identity))
            return result;
        uint primitive = identity.data.x - 1u;
        result.point = geometry::surface_point(restir_endpoint(identity));
        result.hit = identity;
        result.wo = wo;
        result.frontFacing = dot(wo, result.point.normal) >= 0;
        result.texel = material_texel(primitive, asfloat(identity.data.yz));
        result.identity =
            uint4(primitive, identity.data.yz, restir_normal(identity, result.point.normal));
        pt::Hit hit = restir_hit(result);
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
        if (!restir_valid_hit(hit))
            return {};
        float3 position = geometry::surface_point(restir_endpoint(hit)).position;
        float3 delta = origin - position;
        float distance = length(delta);
        // 观察方向决定界面两侧介质和有向发光；重建位置与身份不变。
        return loadSurfaceWithDistance(hit, delta / distance, previous, distance);
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
        RestirReplayCall call = { material_hit(surface), surface.incident, surface.wo, u };
        uint program = materials[material_id(surface.hit.data.x - 1u)].control.x >> 2u;
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        CallShader(program, call);
        return sampled_result(call.hit, call.incident);
    }
    public restir_pt::InitialShading shadeInitial(restir_pt::Surface surface, float3 u,
                                                  float3 direction, bool continuation)
    {
        RestirInitialCall call = { material_hit(surface), surface.incident, surface.wo, u,
                                   direction };
        uint program = (materials[material_id(surface.hit.data.x - 1u)].control.x >> 2u) +
                       (continuation ? 0u : asuint(parameters[0].restir.z));
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        CallShader(program, call);
        return call.result();
    }
    public restir_pt::BsdfEvaluation evaluateBsdf(restir_pt::Surface surface, float3 wi,
                                                  int component)
    {
        RestirEvaluateCall call = { material_hit(surface), surface.wo, wi, surface.incident };
        uint program = (materials[material_id(surface.hit.data.x - 1u)].control.x >> 2u) +
                       asuint(parameters[0].restir.z);
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        CallShader(program, call);
        return { call.hit.texel.xyz, call.hit.texel.w };
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
        return 0.5 / (4.0 * atmosphere::PI) + 0.5 * sun;
    }
    public restir_pt::LightSample sampleLight(restir_pt::Surface surface, float3 u)
    {
        ModScene scene;
        bool sky = parameters[0].lighting.w > 0;
        bool hasLocal = light_count() > 0;
        float probability = sky ? (hasLocal ? 0.5 : 0.0) : 1.0;
        restir_pt::LightSample result = {};
        if (u.z < probability)
        {
            u.z /= probability;
            pt::LightSample quad = scene.sample_light(surface.point, u);
            if (quad.pdf <= 0)
                return result;
            result.hit = restir_stable_hit(light_meta[quad.emission_reference].w * 2u +
                                               (asuint(quad.emission.z) & 1u),
                                           quad.emission.xy);
            result.direction = quad.direction;
            result.distance = quad.distance;
            result.pdf = probability * quad.pdf;
            result.radiance = scene.light_emission(surface.point, quad, false);
            result.lightType = restir_pt::LIGHT_EMISSIVE;
        }
        else if (sky)
        {
            float3 direction;
            if (u.x < 0.5)
                direction = scene.sample_sun(surface.point, float2(u.x * 2.0, u.y)).direction;
            else
            {
                float z = 3.0 - 4.0 * u.x;
                float r = sqrt(max(0.0, 1.0 - z * z));
                float phi = 2.0 * atmosphere::PI * u.y;
                direction = float3(r * cos(phi), z, r * sin(phi));
            }
            pt::Environment radiance = scene.environment(direction, surface.point.position, 1);
            result.direction = direction;
            result.distance = pt::FAR;
            result.pdf = (1.0 - probability) * environmentPdf(direction);
            result.radiance = radiance.sky + radiance.sun;
            result.lightType = restir_pt::LIGHT_ENV;
        }
        return result;
    }
    public float lightPdf(restir_pt::Surface source, restir_pt::HitInfo light, float3 direction,
                          uint type, bool previous)
    {
        ModScene scene;
        bool sky = parameters[0].lighting.w > 0;
        float probability = sky ? (light_count() > 0 ? 0.5 : 0.0) : 1.0;
        if (type == restir_pt::LIGHT_EMISSIVE)
        {
            if (!restir_valid_hit(light))
                return 0;
            return probability *
                   scene.emission_pdf(restir_hit(loadSurface(light, -direction, previous)),
                                      source.point.position);
        }
        return (1.0 - probability) * environmentPdf(direction);
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
