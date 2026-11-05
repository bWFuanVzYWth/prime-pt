/// Minecraft 捕获网格到引擎 PT 契约；源模型、atlas 和 tint 的策略只在此求值。
module mod_scene;

import pt.scene;
import pt.integrator;
import pt.transport;
import pt.roulette;
import mod_call;
__exported import mod_materials;
import mod_lights;
import pt.reconstruction;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.specialized;
import bsdf.openpbr.flow;
import params;
import atmosphere.parameters;
import atmosphere.sampling;
import color.primaries;

#ifndef OPENPBR_VARIANT
#define OPENPBR_VARIANT 0xffffffffu
#endif

[[vk::binding(0, 0)]]
public RaytracingAccelerationStructure scene;
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
[[vk::binding(36, 0)]]
StructuredBuffer<uint> instance_quad_bases;

/// 实例自定义索引只选择对应区块的属性起点。每个 quad 在 BLAS 中占两个三角形。
public uint region_primitive(uint instance, uint local_primitive)
{
    return instance_quad_bases[instance] * 2u + local_primitive;
}

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
    // 辐照度按太阳盘投影立体角换算，采样 PDF 则使用未投影立体角。
    float sine = sin(p.sun.z);
    float projected_solid_angle = atmosphere::PI * sine * sine;
    return max(color::rec2020_to_bt709(rgb * parameters[0].atmosphere_settings.xyz /
                                       projected_solid_angle),
               0);
}

/// 主查询只交接硬件命中引用；OpenPBR 翻译在消费交点时执行。
public struct TracePayload
{
    public uint primitive;
    public float2 bary;
    public float distance;
    public geometry::SurfacePoint surface;
    public uint shading_normal;
};

/// 原有公开texel入口保留，材质、覆盖和光源复用同一UV/tint翻译。
public float4 texel(uint primitive, float2 barycentrics)
{
    return material_texel(primitive, barycentrics);
}

public void finish_vertex(inout pt::TransportCall data)
{
    // 与材质键无关的收尾不扩大每个callable；采样身份无需跨call复制临时随机状态。
    uint status = data.path.status;
    if (status == pt::PATH_ACTIVE)
    {
        if (max(data.path.throughput.x, max(data.path.throughput.y, data.path.throughput.z)) <= 0)
            status = pt::PATH_DONE;
        else if (data.path.depth + 1 >= 2)
        {
            float survival = pt::roulette_probability(data.path.throughput, data.path.eta_scale);
            pt::PathSampler sampler = pt::path_sampler(
                data.pixel, data.sample, parameters[0].options.z, parameters[0].options.x);
            if (survival == 0 || (survival < 1 && sampler.roulette(data.path.depth) >= survival))
                status = pt::PATH_DONE;
            else if (survival < 1)
                data.path.throughput = pt::roulette_reweight(data.path.throughput, survival);
        }
    }
    pt::finish(data.path, status);
}

/// 资源世代的完整材质在消费交点时翻译；几何误差与着色法线独立。
public struct ModScene : pt::ITraceScene, pt::IGuideScene, pt::IFlowScene
{
    public pt::GuidePose guide_pose(pt::Hit hit)
    {
        // 捕获网格没有跨帧实例/顶点对应；保留相机代理，不猜测物体静止。
        return { hit.surface.position, false, false };
    }

    public pt::SurfaceMaterial guide_material(pt::Hit hit, float4 incident)
    {
        hit.texel = texel(hit.identity.x, asfloat(hit.identity.yz));
        return resolve_material(hit, ~openpbr::FLOW_EMISSION, ~0u, 1.0);
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
        hit.identity = uint4(payload.primitive, asuint(payload.bary), payload.shading_normal);
        hit.capability = 0;
        hit.distance = payload.distance;
        hit.front = dot(direction, hit.surface.normal) < 0;
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
        data.path.throughput *= pt::segment_transmittance(data.path.medium.yzw, data.hit.distance);
        // GPU派生表的高30位为资源期按真实Flow去重后的callable槽；不是源OpenPBR flags。
        uint program = materials[material_id(data.hit.identity.x)].control.x >> 2u;
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        ModTransportCall call = ModTransportCall.input(data);
        CallShader(program, call);
        data.path = call.result();
        finish_vertex(data);
    }

    public pt::SurfaceMaterial resolve(pt::Hit hit, uint capability, float4 incident)
    {
        hit.texel = texel(hit.identity.x, asfloat(hit.identity.yz));
        return resolve_material(hit, ~0u, ~0u, parameters[0].block_emission_scale);
    }

    public pt::SurfaceMaterial resolve_flow(pt::Hit hit, uint flow, float4 incident)
    {
        // 光学介质不以atlas颜色调制；由材质键裁掉无消费者的UV/颜色/tint读取。
        if ((flow & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                     openpbr::FLOW_CONDUCTOR_REFLECTION | openpbr::FLOW_EMISSION)) != 0)
            hit.texel = texel(hit.identity.x, asfloat(hit.identity.yz));
        return resolve_material(hit, flow, OPENPBR_VARIANT, parameters[0].block_emission_scale);
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
        return sample_quad_light(point, sample, parameters[0]);
    }

    public float3 light_emission(geometry::SurfacePoint point, pt::LightSample sample, bool solar)
    {
        return solar ? solar_radiance(sample.direction, altitude(point.position.y))
                     : quad_light_emission(sample, parameters[0].block_emission_scale);
    }

    public float emission_pdf(pt::Hit hit, float3 previous)
    {
        return quad_emission_pdf(hit, previous);
    }

    public bool occluded(geometry::Ray connection)
    {
        RayDesc ray = { connection.origin, connection.t_min, connection.direction,
                        connection.t_max };
        if (COVERAGE_MASK == 0)
        {
            RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_FORCE_OPAQUE> opaque;
            opaque.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, ray);
            opaque.Proceed();
            return opaque.CommittedStatus() != COMMITTED_NOTHING;
        }
        RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH> query;
        query.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, ray);
        while (query.Proceed())
        {
            // RayQuery 不执行 any-hit；候选必须使用与主查询相同的覆盖规则。
            if (query.CandidateType() == CANDIDATE_NON_OPAQUE_TRIANGLE &&
                material_accepts(
                    region_primitive(query.CandidateInstanceID(), query.CandidatePrimitiveIndex()),
                    query.CandidateTriangleBarycentrics(), query.CandidateTriangleFrontFace()))
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
