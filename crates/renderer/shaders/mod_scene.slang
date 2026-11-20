/// Minecraft 捕获网格到引擎 PT 契约；源模型、atlas 和 tint 的策略只在此求值。
module mod_scene;

#ifndef PRIMEPT_COVERAGE_MASK
#define PRIMEPT_COVERAGE_MASK 40
#endif

import pt.scene;
import pt.integrator;
import transport.roulette;
import mod_call;
__exported import mod_materials;
import mod_lights;
import mod_decode;
import pt.reconstruction;
import geometry.ray;
import sampling.cone;
import openpbr.api;
import openpbr.specialized;
import openpbr.flow;
import params;
import atmosphere.parameters;
import atmosphere.sampling;
import color.primaries;
import light_sampling.sample;
import transport.surface_material;
import transport.medium;

#ifndef OPENPBR_VARIANT
#define OPENPBR_VARIANT 0xffffffffu
#endif

[[vk::binding(0, 0)]]
public var scene : RaytracingAccelerationStructure;
[[vk::binding(4, 0)]]
public var parameters : StructuredBuffer<Params>;
[[vk::binding(5, 0)]]
var energy_texture : Texture3D<float4>;
[[vk::binding(6, 0)]]
var linear_sampler : SamplerState;
[[vk::binding(16, 0)]]
var sky_mapping : StructuredBuffer<float4>;
[[vk::binding(17, 0)]]
var sky_texture : Texture2D<float4>;
[[vk::binding(18, 0)]]
var optical_depth : Texture2D<float4>;
[[vk::binding(36, 0)]]
var instance_quad_bases : StructuredBuffer<uint>;
[[vk::binding(37, 0)]]
var resident_quad_tokens : StructuredBuffer<uint2>;

/// 当前驻留 quad 的 64 位身份与其有向半面；各帧的 primitive 槽可不同。
public func stable_triangle(primitive: uint)->uint3
{
    return uint3(resident_quad_tokens[primitive >> 1u], primitive & 1u);
}

/// 实例自定义索引只选择对应区块的属性起点。每个 quad 在 BLAS 中占两个三角形。
public func region_primitive(instance: uint, local_primitive: uint)->uint
{
    return instance_quad_bases[instance] * 2u + local_primitive;
}

public func sun_contains(direction: float3)->bool
{
    return sampling::cone_contains_chord(parameters[0].lighting.xyz,
                                         asfloat(parameters[0].programs.w), direction);
}

public func sun_pdf()->float
{
    return sampling::uniform_cone_pdf_chord(asfloat(parameters[0].programs.w));
}

func altitude(world_y: float)->float
{
    return clamp(0.5 + (world_y - parameters[0].origin.w) * 0.001, 0.0, 36000.0);
}

func solar_radiance(direction: float3, altitude_km: float)->float3
{
    var p : atmosphere::Params = parameters[0].atmosphere;
    p.observer_height_km = altitude_km;
    return max(
        color::rec2020_to_bt709(atmosphere::solar_radiance(
            p, optical_depth, linear_sampler, direction, parameters[0].atmosphere_settings.xyz)),
        0);
}

/// 主查询只交接硬件命中引用；OpenPBR 翻译在消费交点时执行。
public struct TracePayload
{
    public var primitive : uint;
    public var bary : float2;
    public var distance : float;
    public var surface : geometry::SurfacePoint;
    public var shading_normal : uint;
};

/// 原有公开texel入口保留，材质、覆盖和光源复用同一UV/tint翻译。
public func texel(primitive: uint, barycentrics: float2)->float4
{
    return material_texel(primitive, barycentrics);
}

public func finish_vertex(inout data: pt::TransportCall)->void
{
    // 与材质键无关的收尾不扩大每个callable；采样身份无需跨call复制临时随机状态。
    var status : uint = data.path.status;
    if (status == pt::PATH_ACTIVE)
    {
        if (max(data.path.throughput.x, max(data.path.throughput.y, data.path.throughput.z)) <= 0)
            status = pt::PATH_DONE;
        else if (data.path.depth + 1 >= 2)
        {
            let survival : float = transport::roulette_probability(data.path.throughput,
                                                                   data.path.eta_scale);
            let sampler : pt::PathSampler =
                              pt::path_sampler(data.pixel, data.sample, parameters[0].options.z,
                                               parameters[0].options.x);
            if (survival == 0 || (survival < 1 && sampler.roulette(data.path.depth) >= survival))
                status = pt::PATH_DONE;
            else if (survival < 1)
                data.path.throughput = transport::roulette_reweight(data.path.throughput, survival);
        }
    }
    pt::finish(data.path, status);
}

/// 资源世代的完整材质在消费交点时翻译；几何误差与着色法线独立。
public struct ModScene : pt::ITraceScene, pt::IGuideScene, pt::IFlowScene
{
    public func guide_pose(hit: pt::Hit)->pt::GuidePose
    {
        // 捕获网格没有跨帧实例/顶点对应；保留相机代理，不猜测物体静止。
        return { hit.surface.position, false, false };
    }

    public func guide_material(input_hit: pt::Hit, incident: float4)->transport::SurfaceMaterial
    {
        var hit = input_hit;
        hit.texel = texel(hit.identity.x, asfloat(hit.identity.yz));
        return resolve_material(hit, ~openpbr::FLOW_EMISSION, ~0u, 1.0);
    }

    public func intersect(origin: float3, direction: float3, out hit: pt::Hit)->bool
    {
        hit = (pt::Hit)0;
        let ray : RayDesc = { origin, 0, direction, pt::FAR };
        var ray_payload : TracePayload = (TracePayload)0;
        ray_payload.primitive = 0xffffffffu;
        TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, ray_payload);
        if (ray_payload.primitive == 0xffffffffu)
            return false;
        hit.surface = ray_payload.surface;
        hit.identity =
            uint4(ray_payload.primitive, asuint(ray_payload.bary), ray_payload.shading_normal);
        hit.capability = 0;
        hit.distance = ray_payload.distance;
        hit.front = dot(direction, hit.surface.normal) < 0;
        return true;
    }

    public func resolve_hit(inout hit: pt::Hit, incident: float4)->void
    {
    }

    public func prefix(hit: pt::Hit)->bool
    {
        return false;
    }

    public func delta_interface(hit: pt::Hit, incident: float4)->pt::DeltaInterface
    {
        // prefix恒为false，所有材质进入已绑定键的CH/callable。
        return {};
    }

    public func shade(inout data: pt::TransportCall)->void
    {
        data.path.throughput *=
            transport::segment_transmittance(data.path.medium.yzw, data.hit.distance);
        // GPU派生表的高30位为资源期按真实Flow去重后的callable槽；不是源OpenPBR flags。
        let program : uint =
                          material_words.Load(material_id(data.hit.identity.x) * 112u + 64u) >> 2u;
#ifdef PRIMEPT_SER
        ReorderThread(program, 32u);
#endif
        var call : ModTransportCall = ModTransportCall.input(data);
        CallShader(program, call);
        data.path = call.result();
        finish_vertex(data);
    }

    public func resolve(input_hit: pt::Hit, capability: uint, incident: float4)
        ->transport::SurfaceMaterial
    {
        var hit = input_hit;
        hit.texel = texel(hit.identity.x, asfloat(hit.identity.yz));
        return resolve_material(hit, ~0u, ~0u, parameters[0].block_emission_scale);
    }

    public func resolve_flow(hit: pt::Hit, flow: uint, incident: float4)->transport::SurfaceMaterial
    {
#if defined(OPENPBR_FLOW) && OPENPBR_VARIANT != 0xffffffffu
        return decode_surface<AtlasColor, FetchAux>(hit, parameters[0].block_emission_scale, {});
#else
        return resolve_material<false, true>(hit, flow, OPENPBR_VARIANT,
                                             parameters[0].block_emission_scale);
#endif
    }

    public func energy()->openpbr::Energy
    {
        return { energy_texture, linear_sampler };
    }

    public func sample_sun(surface_point: geometry::SurfacePoint, random_sample: float2)
        ->light_sampling::LightSample
    {
        var result : light_sampling::LightSample = (light_sampling::LightSample)0;
        if (parameters[0].lighting.w <= 0)
            return result;
        result.direction = sampling::sample_uniform_cone_chord(
            parameters[0].lighting.xyz, asfloat(parameters[0].programs.w), random_sample);
        result.pdf = sun_pdf();
        result.distance = pt::FAR;
        result.visibility = { geometry::offset_origin(surface_point, result.direction), 0,
                              result.direction, pt::FAR };
        return result;
    }

    public func sample_light(surface_point: geometry::SurfacePoint, random_sample: float3)
        ->light_sampling::LightSample
    {
        return sample_quad_light(surface_point, random_sample, parameters[0]);
    }

    public func light_emission(surface_point: geometry::SurfacePoint,
                               light_sample: light_sampling::LightSample, solar: bool)
        ->float3
    {
        return solar ? solar_radiance(light_sample.direction, altitude(surface_point.position.y))
                     : quad_light_emission(light_sample, parameters[0].block_emission_scale);
    }

    public func emission_pdf(hit: pt::Hit, previous: float3)->float
    {
        return quad_emission_pdf<SelectedEmitterArea>(hit, previous,
                                                      parameters[0].anchor_region.xyz);
    }

    public func occluded(connection: geometry::Ray)->bool
    {
        let ray : RayDesc = { connection.origin, connection.t_min, connection.direction,
                              connection.t_max };
#if PRIMEPT_COVERAGE_MASK == 0
        {
            var opaque : RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_FORCE_OPAQUE>;
            opaque.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, ray);
            opaque.Proceed();
            return opaque.CommittedStatus() != COMMITTED_NOTHING;
        }
#else
        var query : RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH>;
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
#endif
    }

    public func environment(direction: float3, previous: float3, depth: uint)->pt::Environment
    {
        var result : pt::Environment = (pt::Environment)0;
        let p : Params = parameters[0];
        if (p.lighting.w <= 0)
            return result;
        let rgb : float3 =
                      atmosphere::sky_radiance(p.atmosphere, sky_mapping, sky_texture, direction);
        result.sky = max(color::rec2020_to_bt709(rgb * p.atmosphere_settings.xyz), 0);
        if (sun_contains(direction))
        {
            result.sun = solar_radiance(direction, depth == 0 ? p.atmosphere.observer_height_km
                                                              : altitude(previous.y));
            result.sun_pdf = sun_pdf();
        }
        return result;
    }
};
