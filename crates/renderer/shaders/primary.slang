/// 只负责硬件命中和 K1/K2 交接；路径推进完全使用引擎 pt.integrator。
module primary;
#include "opacity/micromap.inc.slang"
#ifndef PRIMEPT_OMM
#define PRIMEPT_OMM 0
#endif
import mod_scene;
import mod_call;
import nrd_path;
import nrd_guides;
import pt.material_vertex;
import mod_guides;
import params;
import pt.integrator;
import pt.reconstruction;
import geometry.ray;
import camera.pinhole;
import openpbr.specialized;
import openpbr.flow;
import light_sampling.sample;
import transport.medium;

#ifndef OPENPBR_FLOW
#define OPENPBR_FLOW 131
#endif
#ifndef PRIMEPT_LIGHT_MASK
#define PRIMEPT_LIGHT_MASK 3
#endif
#ifndef PRIMEPT_AUTHORED_NORMAL
#define PRIMEPT_AUTHORED_NORMAL 1
#endif

[[vk::binding(7, 0)]]
var output_image : RWStructuredBuffer<float4>;
[[vk::binding(8, 0)]]
var accumulation : RWStructuredBuffer<float4>;
[[vk::binding(9, 0)]]
var sample_counts : RWStructuredBuffer<uint>;

/// prefix=false交接：几何、方向、状态和稳定identity采用独立plane。
/// 两个16B plane、一个8B plane、两个4B plane和独立identity；不交接材质纹素。
/// position/offset、normal、direction、distance 保留原 FP32；quad 任意朝向和非共面。
/// throughput=1、eta=1、medium=相机实际介质、depth/events=0 来自 K1 不消费顶点，
/// 非体素假设；材质纹素由叶子按键读取，不跨K1/K2交接。
[[vk::binding(10, 0)]]
var landing_position : RWStructuredBuffer<float4>;
[[vk::binding(11, 0)]]
var landing_normal_direction_x : RWStructuredBuffer<float4>;
[[vk::binding(12, 0)]]
var landing_header : RWStructuredBuffer<uint>;
[[vk::binding(13, 0)]]
var landing_direction_yz : RWStructuredBuffer<float2>;
[[vk::binding(14, 0)]]
var landing_distance : RWStructuredBuffer<float>;
[[vk::binding(15, 0)]]
var prefix_image : RWStructuredBuffer<float4>;

[[vk::binding(26, 0)]]
var landing_identity : RWStructuredBuffer<uint4>;

[shader("miss")]
func primary_miss(inout ray_payload: TracePayload)->void
{
    ray_payload.primitive = 0xffffffffu;
}

[shader("anyhit")]
func primary_anyhit(inout ray_payload: TracePayload,
                    attributes: BuiltInTriangleIntersectionAttributes)
    ->void
{
    if (!material_accepts(region_primitive(InstanceID(), PrimitiveIndex()), attributes.barycentrics,
                          HitKind() == 0xfeu))
        IgnoreHit();
}

[shader("closesthit")]
func primary_closesthit(inout ray_payload: TracePayload,
                        attributes: BuiltInTriangleIntersectionAttributes)
    ->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(primary_closesthit)
#endif
    ray_payload.primitive = region_primitive(InstanceID(), PrimitiveIndex());
    ray_payload.bary = attributes.barycentrics;
    ray_payload.distance = RayTCurrent();
    let a : float3 = HitTriangleVertexPosition(0);
    let b : float3 = HitTriangleVertexPosition(1);
    let c : float3 = HitTriangleVertexPosition(2);
    ray_payload.surface = geometry::triangle_point(a, b, c, attributes.barycentrics,
                                                   ObjectToWorld3x4(), WorldToObject3x4());
    ray_payload.shading_normal = 0;
    if (PRIMEPT_AUTHORED_NORMAL != 0)
        ray_payload.shading_normal =
            mapped_shading_normal(ray_payload.primitive, ray_payload.bary, b - a, c - a,
                                  (float3x3)ObjectToWorld3x4(), ray_payload.surface.normal);
}

func store_landing(pixel: uint, data: pt::TransportCall)->void
{
    let flags : uint = data.hit.capability | (uint(data.hit.front) << 8) | (data.path.status << 16);
    landing_header[pixel] = flags;
    if (data.path.status != pt::PATH_ACTIVE)
        return;
    landing_position[pixel] = float4(data.hit.surface.position, data.hit.surface.offset);
    landing_normal_direction_x[pixel] = float4(data.hit.surface.normal, data.path.direction.x);
    landing_direction_yz[pixel] = data.path.direction.yz;
    landing_distance[pixel] = data.hit.distance;
    landing_identity[pixel] = data.hit.identity;
}

func load_landing(input_pixel: uint, coordinate: uint2, flags: uint)->pt::TransportCall
{
    var pixel = input_pixel;
    var data : pt::TransportCall = (pt::TransportCall)0;
    let position : float4 = landing_position[pixel];
    let normal_direction : float4 = landing_normal_direction_x[pixel];
    data.hit.surface = { position.xyz, normal_direction.xyz, position.w };
    data.hit.capability = flags & 255u;
    data.hit.front = (flags & 256u) != 0;
    data.hit.distance = landing_distance[pixel];
    data.hit.identity = landing_identity[pixel];
    data.path.direction = float3(normal_direction.w, landing_direction_yz[pixel]);
    data.path.eta_scale = 1;
    data.path.throughput = 1;
    data.path.medium = parameters[0].initial_medium;
    data.pixel = coordinate;
    let count : uint = sample_counts[pixel];
    data.sample = parameters[0].rr_sample +
                  (count >= parameters[0].image.z ? count - parameters[0].image.z : 0u);
    return data;
}

[shader("raygeneration")]
func prefix_main()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(prefix_main)
#endif
    let params : Params = parameters[0];
    let coordinate : uint2 = DispatchRaysIndex().xy;
    var pixel : uint = coordinate.y * params.image.x + coordinate.x;
    var data : pt::TransportCall = (pt::TransportCall)0;
    data.pixel = coordinate;
    let diagnostic : bool = (params.output_size.z & 1u) != 0;
    let count : uint = sample_counts[pixel];
    data.sample = params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u);
    if (!diagnostic && accumulation[pixel].w < 0)
    {
        data.path.status =
            accumulation[pixel].w == -3 ? pt::PATH_DIELECTRIC_LIMIT : pt::PATH_INVALID;
        store_landing(pixel, data);
        prefix_image[pixel] = 0;
        return;
    }
    let sampler : pt::PathSampler =
                      pt::path_sampler(coordinate, data.sample, params.options.z, params.options.x);
    let offset : float2 =
                     params.reconstruction.w > 0 ? 0.5 + params.reconstruction.xy : sampler.film();
    let film = camera::camera_sample_uv_offset(coordinate, params.inverse_extent, offset);
    data.path.origin = params.origin.xyz;
    data.path.direction =
        camera::camera_scaled_direction(params.forward.xyz, params.right.xyz, params.up.xyz, film);
    data.path.throughput = 1;
    data.path.eta_scale = 1;
    data.path.medium = parameters[0].initial_medium;
    var distance : float;
    var provider : ModScene;
    if (params.reconstruction.w == 3)
    {
        pt::trace_prefix(provider, data, settings(params), distance);
        if (distance == pt::FAR)
            sky_signal(coordinate);
    }
    else if (params.reconstruction.w > 0)
    {
        let previous : camera::Camera = { params.previous_position, params.previous_forward,
                                          params.previous_right, params.previous_up };
        let current = params.current_camera;
        let guides : Guides = { coordinate, film, current, previous };
        pt::trace_prefix(provider, data, settings(params), distance, guides);
    }
    else
        pt::trace_prefix(provider, data, settings(params), distance);
    store_landing(pixel, data);
    prefix_image[pixel] = float4(data.path.radiance, distance);
}

[shader("raygeneration")]
func render_main()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_main)
#endif
    let params : Params = parameters[0];
    let coordinate : uint2 = DispatchRaysIndex().xy;
    let pixel : uint = coordinate.y * params.image.x + coordinate.x;
    let header : uint = landing_header[pixel];
    let status : uint = (header >> 16) & 3u;
    if (status != pt::PATH_ACTIVE)
    {
        output_image[pixel] = float4(0, 0, 0,
                                     status == pt::PATH_INVALID            ? -1
                                     : status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                           : 0);
        return;
    }
    var data : pt::TransportCall = load_landing(pixel, coordinate, header);
    var provider : ModScene;
    if (params.reconstruction.w == 3)
    {
        let start = material_id(data.hit.identity.x) * 112u + 64u;
        if (data.sample == params.rr_sample &&
            fixed_guide_candidate(material_words.Load(start + 8u) >> 20))
        {
            data.path.origin = params.origin.xyz;
            write_fixed_guides(provider, data);
        }
        let program = material_words.Load(start) >> 2;
        var first = ModTransportCall.input(data);
        CallShader(program, first);
        data.path = first.result();
        let before = data.path.radiance;
        data.path.radiance = 0;
        let direction = data.path.direction;
        var distance = pt::FAR;
        if (data.path.status == pt::PATH_ACTIVE)
        {
            if (provider.intersect(data.path.origin, direction, data.hit))
            {
                provider.resolve_hit(data.hit, data.path.medium);
                distance = pt::trace_tail<ModScene, true>(provider, data);
            }
            else
                pt::escape(provider, data.path);
        }
        finish_signal(pixel * 2 + (data.sample - params.rr_sample), max(data.path.radiance, 0),
                      direction, distance);
        data.path.radiance += before;
    }
    else
        pt::trace_tail(provider, data);
    output_image[pixel] =
        float4(data.path.radiance, data.path.status == pt::PATH_INVALID            ? -1
                                   : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                   : 0);
}

[shader("callable")]
func shade_opaque(inout call: ModTransportCall)->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_opaque)
#endif
    var data : pt::TransportCall = call.unpack();
    var provider : ModScene;
    if (parameters[0].reconstruction.w == 3 && data.path.depth == 0)
    {
        let lane = data.sample - parameters[0].rr_sample;
        let pixel = data.pixel.y * parameters[0].image.x + data.pixel.x;
        let mat = provider.resolve_flow(data.hit, OPENPBR_FLOW, data.path.medium);
        var first = pt::MaterialCall::primary_input(data.sample, lane, data.path.radiance);
        first.initialize_primary(data.path.medium);
        data.hit.distance = length(data.hit.surface.position - parameters[0].origin.xyz);
        var signals : pt::PrimaryLighting;
        pt::shade_material_observed<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, true, true>(
            provider, first, data.hit, mat, data.path.direction, settings(parameters[0]), lane,
            signals);
        let blocker = begin_signal<OPENPBR_FLOW>(pixel * 2 + lane, signals, data.hit, mat,
                                                 data.path.medium, -data.path.direction);
        if (blocker == pt::FAR)
            first.radiance += signals.sun;
        call.store<OPENPBR_FLOW>(first.result());
        return;
    }
    pt::process_flow<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, false>(provider, data,
                                                                        settings(parameters[0]));
    call.store<OPENPBR_FLOW>(data.path);
}

// 0=原调度，1=同raygen的callable对照，2=Hit Group/RayQuery，3=Hit Group/独立遍历。
#ifndef PRIMEPT_DISPATCH
#define PRIMEPT_DISPATCH 0
#endif

[shader("closesthit")]
func shade_hit(inout call: ModTransportCall, attributes: BuiltInTriangleIntersectionAttributes)
    ->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_hit)
#endif
#ifdef PRIMEPT_HIT_STUB
    call.state = pt::PATH_DONE;
    call.surface_point.position = 0;
    call.surface_point.normal = 0;
    call.identity.xyz = 0;
    call.radiance = 1;
#else
    var data : pt::TransportCall = call.unpack();
    var provider : ModScene;
    var weights : pt::DirectWeights;
    pt::process_flow_vertex<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, false, true>(
        provider, data, settings(parameters[0]), weights);
    call.store<OPENPBR_FLOW>(data.path);
    // 输入已消费的normal/identity复用为24B灯权重，不扩大payload。
    call.surface_point.normal = weights.sun;
    call.identity.xyz = asuint(weights.lamp);
#endif
}

[shader("raygeneration")]
func prefix_empty()
    ->void {
#if PRIMEPT_OMM
        ENGINE_OPACITY_MICROMAP(prefix_empty)
#endif
    }

func shadow_occluded(connection: geometry::Ray)
    ->bool
{
    if (PRIMEPT_DISPATCH == 3)
    {
        let ray : RayDesc = { connection.origin, connection.t_min, connection.direction,
                              connection.t_max };
        var unused : uint = 0;
        var shadow : HitObject = HitObject::TraceRay(scene,
                                                     RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH |
                                                         RAY_FLAG_SKIP_CLOSEST_HIT_SHADER,
                                                     0xff, 0, 1, 0, ray, unused);
        return shadow.IsHit();
    }
    return ModScene().occluded(connection);
}

func object_hit(object: HitObject, direction: float3)->pt::Hit
{
    let attributes : BuiltInTriangleIntersectionAttributes =
                         object.GetAttributes<BuiltInTriangleIntersectionAttributes>();
    let vertices : float3[3] = object.GetTriangleVertexPositions();
    // EXT返回object-space顶点，与position-fetch closest-hit采用相同误差界。
    var hit : pt::Hit = {};
    hit.surface = geometry::triangle_point(
        vertices[0], vertices[1], vertices[2], attributes.barycentrics,
        transpose(object.GetObjectToWorld()), transpose(object.GetWorldToObject()));
    let primitive : uint = region_primitive(object.GetInstanceID(), object.GetPrimitiveIndex());
    let normal : uint = mapped_shading_normal(primitive, attributes.barycentrics,
                                              vertices[1] - vertices[0], vertices[2] - vertices[0],
                                              (float3x3)transpose(object.GetObjectToWorld()),
                                              hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = object.GetRayDesc().TMax;
    hit.front = dot(direction, hit.surface.normal) < 0;
    return hit;
}

[shader("raygeneration")]
func render_fused()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_fused)
#endif
    let params : Params = parameters[0];
    let coordinate : uint2 = DispatchRaysIndex().xy;
    var pixel : uint = coordinate.y * params.image.x + coordinate.x;
    var data : pt::TransportCall = {};
    data.pixel = coordinate;
    let count : uint = sample_counts[pixel];
    let diagnostic : bool = (params.output_size.z & 1u) != 0;
    data.sample = params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u);
    if (!diagnostic && accumulation[pixel].w < 0)
    {
        output_image[pixel] = float4(0, 0, 0, accumulation[pixel].w);
        return;
    }
    let sampler : pt::PathSampler =
                      pt::path_sampler(coordinate, data.sample, params.options.z, params.options.x);
    let offset : float2 =
                     params.reconstruction.w > 0 ? 0.5 + params.reconstruction.xy : sampler.film();
    let film = camera::camera_sample_uv_offset(coordinate, params.inverse_extent, offset);
    data.path.origin = params.origin.xyz;
    data.path.direction =
        camera::camera_scaled_direction(params.forward.xyz, params.right.xyz, params.up.xyz, film);
    data.path.throughput = 1;
    data.path.eta_scale = 1;
    data.path.medium = params.initial_medium;
    var provider : ModScene;
    var distance : float = pt::FAR;
    while (data.path.status == pt::PATH_ACTIVE)
    {
        let ray : RayDesc = { data.path.origin, 0, data.path.direction, pt::FAR };
        var unused : uint = 0;
        var found : HitObject =
                        HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
        if (!found.IsHit())
        {
            pt::escape(provider, data.path);
            break;
        }
        data.hit = object_hit(found, data.path.direction);
        if (data.path.depth == 0)
            distance = data.hit.distance;
        if (PRIMEPT_DISPATCH == 1)
        {
            provider.shade(data);
            continue;
        }
        data.path.throughput *=
            transport::segment_transmittance(data.path.medium.yzw, data.hit.distance);
        let program : uint =
                          material_words.Load(material_id(data.hit.identity.x) * 112u + 64u) >> 2u;
        found.SetShaderTableIndex(program + 1u);
#ifdef PRIMEPT_SER
        ReorderThread(found, 0u, 0u);
#endif
        var call : ModTransportCall = ModTransportCall.input(data);
        HitObject::Invoke(found, call);
        // 只保活几何点和采样身份，灯样本不跨SER/材质阶段。
        var light : light_sampling::LightSample;
        var direct : float3 = 0;
        if (any(call.surface_point.normal > 0))
        {
            light = provider.sample_sun(data.hit.surface, sampler.sun(data.path.depth));
            if (!shadow_occluded(light.visibility))
                direct += call.surface_point.normal *
                          provider.light_emission(data.hit.surface, light, true);
        }
        let lamp : float3 = asfloat(call.identity.xyz);
        if (any(lamp > 0))
        {
            light = provider.sample_light(data.hit.surface, sampler.light(data.path.depth));
            if (!shadow_occluded(light.visibility))
                direct += lamp * provider.light_emission(data.hit.surface, light, false);
        }
        data.path = call.result();
        data.path.radiance += direct;
        finish_vertex(data);
    }
    prefix_image[pixel] = float4(0, 0, 0, distance);
    output_image[pixel] =
        float4(data.path.radiance, data.path.status == pt::PATH_INVALID            ? -1
                                   : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                   : 0);
}
