/// Minecraft 捕获网格到引擎 PT 契约；源模型、atlas 和 tint 的策略只在此求值。
module mod_scene;

import pt.scene;
import pt.reconstruction;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.specialized;
import bsdf.openpbr.flow;
import params;
import atmosphere.parameters;
import atmosphere.sampling;
import color.primaries;

[[vk::binding(0, 0)]]
public RaytracingAccelerationStructure scene;
[[vk::binding(1, 0)]]
StructuredBuffer<uint4> quads;
[[vk::binding(2, 0)]]
Texture2D<float4> atlas;
[[vk::binding(3, 0)]]
SamplerState atlas_sampler;
[[vk::binding(4, 0)]]
public StructuredBuffer<Params> parameters;
[[vk::binding(5, 0)]]
Texture3D<float4> energy_texture;
[[vk::binding(6, 0)]]
SamplerState linear_sampler;
[[vk::binding(16, 0)]]
StructuredBuffer<float4> sky_mapping;
[[vk::binding(17, 0)]]
Texture2D<float4> sky_texture;
[[vk::binding(18, 0)]]
Texture2D<float4> optical_depth;

float sun_cos()
{
    return cos(parameters[0].atmosphere.sun.z);
}

float sun_pdf()
{
    return 1.0 / (2.0 * atmosphere::PI * (1.0 - sun_cos()));
}

float altitude(float world_y)
{
    return clamp(0.5 + (world_y - parameters[0].origin.w) * 0.001, 0.0, 36000.0);
}

float3 solar_radiance(float3 direction, float altitude_km)
{
    atmosphere::Params p = parameters[0].atmosphere;
    p.view.w = altitude_km;
    float4 spectrum =
        p.solar * atmosphere::view_transmittance(p, optical_depth, linear_sampler, direction);
    float3 rgb = spectrum.x * p.rgb[0].xyz + spectrum.y * p.rgb[1].xyz + spectrum.z * p.rgb[2].xyz +
                 spectrum.w * p.rgb[3].xyz;
    return max(color::rec2020_to_bt709(rgb * parameters[0].atmosphere_settings.xyz * sun_pdf()), 0);
}

/// 主查询只交接硬件命中引用；OpenPBR 翻译在消费交点时执行。
public struct TracePayload
{
    public uint primitive;
    public float2 bary;
    public float distance;
    public geometry::SurfacePoint surface;
};

float3 to_linear(float3 value)
{
    return select(value <= 0.04045, value / 12.92, pow((value + 0.055) / 1.055, 2.4));
}

/// 三个 16B 块为 UV32、共享 tint/material16；位置强制从 AS position fetch 获取。
/// alpha 路径仅消费前两块；无 index、字典或位置重建，三角面保留退化第二槽。
public float4 texel(uint primitive, float2 barycentrics)
{
    uint base = (primitive >> 1) * 3;
    bool second = (primitive & 1u) != 0;
    float4 uv01 = asfloat(quads[base]), uv23 = asfloat(quads[base + 1]);
    float3 bary = float3(1.0 - barycentrics.x - barycentrics.y, barycentrics);
    float2 uv = 0;
    uv += uv01.xy * bary.x;
    uv += select(second, uv23.xy, uv01.zw) * bary.y;
    uv += select(second, uv23.zw, uv23.xy) * bary.z;
    float3 shared_tint = asfloat(quads[base + 2].xyz);
    float3 tint = 0;
    // 保持旧三个顶点的 FP32 插值算序；颜色常量折叠另作数值优化。
    tint += shared_tint * bary.x;
    tint += shared_tint * bary.y;
    tint += shared_tint * bary.z;
    float4 sampled = atlas.SampleLevel(atlas_sampler, uv, 0);
    return float4(sampled.rgb * to_linear(saturate(tint)), sampled.a);
}

/// 只支持空气中的不透明 roughness 预设；alpha 是覆盖规则，不是透射闭包。
/// 不依赖体素或轴向。添加介质、发光、delta 前缀等能力时须扩展场景和 landing ABI，
/// 不可沿用当前常量状态恢复；通用引擎 PT 算法不承担这些 mod 策略。
public struct ModScene : pt::ITraceScene, pt::IGuideScene, pt::IFlowScene
{
    public pt::GuidePose guide_pose(pt::Hit hit)
    {
        // 捕获网格没有跨帧实例/顶点对应；保留相机代理，不猜测物体静止。
        return { hit.surface.position, false, false };
    }

    public pt::SurfaceMaterial guide_material(pt::Hit hit, float4 incident)
    {
        return resolve(hit, 0, incident);
    }

    public bool intersect(float3 origin, float3 direction, out pt::Hit hit)
    {
        hit = (pt::Hit)0;
        RayDesc ray = { origin, 0, direction, pt::FAR };
        TracePayload payload = (TracePayload)0;
        payload.primitive = 0xffffffffu;
        TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, payload);
        if (payload.primitive == 0xffffffffu)
            return false;
        hit.surface = payload.surface;
        uint base = (payload.primitive >> 1) * 3;
        hit.identity = uint4(payload.primitive, asuint(payload.bary), 0);
        hit.capability = quads[base + 2].w;
        hit.distance = payload.distance;
        hit.front = dot(direction, hit.surface.normal) < 0;
        hit.texel = texel(payload.primitive, payload.bary);
        return true;
    }

    public void resolve_hit(inout pt::Hit hit, float4 incident)
    {
    }

    public bool prefix(pt::Hit hit)
    {
        return false;
    }

    public void shade(inout pt::TransportCall data)
    {
#ifdef PRIMEPT_SER
        // 所有命中进入同一个 callable；先仅允许实现按后续调用重排。
        ReorderThread(0, 0u);
#endif
        CallShader(0, data);
    }

    public pt::SurfaceMaterial resolve(pt::Hit hit, uint capability, float4 incident)
    {
        pt::SurfaceMaterial material = (pt::SurfaceMaterial)0;
        material.pbr.base_roughness = float4(hit.texel.rgb, select(hit.capability == 1, 0.25, 0.9));
        material.pbr.surface = float4(1.5, 0, 0, 0);
        material.transmitted = incident;
        material.boundary_filter = 1;
        return material;
    }

    public pt::SurfaceMaterial resolve_flow(pt::Hit hit, uint flow, float4 incident)
    {
        pt::SurfaceMaterial material = resolve(hit, 0, incident);
        if ((flow & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                     openpbr::FLOW_CONDUCTOR_REFLECTION)) == 0)
            material.pbr.base_roughness.xyz = 0;
        return material;
    }

    public openpbr::Energy energy()
    {
        return { energy_texture, linear_sampler };
    }

    public pt::LightSample sample_sun(geometry::SurfacePoint point, float2 sample)
    {
        pt::LightSample result = (pt::LightSample)0;
        if (parameters[0].lighting.w <= 0)
            return result;
        float3 normal = parameters[0].lighting.xyz;
        float z = lerp(1.0, sun_cos(), sample.x);
        float radius = sqrt(max(0.0, 1.0 - z * z));
        float phi = 2.0 * atmosphere::PI * sample.y;
        float3 tangent =
            normalize(cross(abs(normal.z) < 0.999 ? float3(0, 0, 1) : float3(0, 1, 0), normal));
        result.direction =
            normal * z + radius * (cos(phi) * tangent + sin(phi) * cross(normal, tangent));
        result.pdf = sun_pdf();
        result.distance = pt::FAR;
        result.visibility = { geometry::offset_origin(point, result.direction), 0, result.direction,
                              pt::FAR };
        return result;
    }

    public pt::LightSample sample_light(geometry::SurfacePoint point, float3 sample)
    {
        return (pt::LightSample)0;
    }

    public float3 light_emission(geometry::SurfacePoint point, pt::LightSample sample, bool solar)
    {
        return solar ? solar_radiance(sample.direction, altitude(point.position.y))
                     : sample.emission;
    }

    public float emission_pdf(pt::Hit hit, float3 previous)
    {
        return 0;
    }

    public bool occluded(geometry::Ray connection)
    {
        RayDesc ray = { connection.origin, connection.t_min, connection.direction,
                        connection.t_max };
        RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> query;
        query.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, ray);
        while (query.Proceed())
        {
            // RayQuery 不执行 any-hit；候选必须使用与主查询相同的覆盖规则。
            if (query.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE &&
                texel(query.CandidatePrimitiveIndex() + query.CandidateInstanceID() * 2u,
                      query.CandidateTriangleBarycentrics())
                        .a >= 0.5)
                query.CommitNonOpaqueTriangleHit();
        }
        return query.CommittedStatus() != COMMITTED_NOTHING;
    }

    public pt::Environment environment(float3 direction, float3 previous, uint depth)
    {
        pt::Environment result = (pt::Environment)0;
        Params p = parameters[0];
        if (p.lighting.w <= 0)
            return result;
        float3 rgb = atmosphere::sky_radiance(p.atmosphere, sky_mapping, sky_texture, direction);
        result.sky = max(color::rec2020_to_bt709(rgb * p.atmosphere_settings.xyz), 0);
        if (dot(direction, p.lighting.xyz) >= sun_cos())
        {
            result.sun =
                solar_radiance(direction, depth == 0 ? p.atmosphere.view.w : altitude(previous.y));
            result.sun_pdf = sun_pdf();
        }
        return result;
    }
};
