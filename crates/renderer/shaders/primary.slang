/// 只负责硬件命中和 K1/K2 交接；路径推进完全使用引擎 pt.integrator。
module primary;
#include "geometry/opacity.slang"
#ifndef PRIMEPT_OMM
#define PRIMEPT_OMM 0
#endif
import mod_scene;
import mod_call;
import mod_guides;
import params;
import pt.integrator;
import pt.transport;
import pt.denoise;
import pt.reconstruction;
import geometry.ray;
import geometry.camera;
import bsdf.openpbr.specialized;
import bsdf.openpbr.flow;

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
RWStructuredBuffer<float4> output_image;
[[vk::binding(8, 0)]]
RWStructuredBuffer<float4> accumulation;
[[vk::binding(9, 0)]]
RWStructuredBuffer<uint> sample_counts;

/// prefix=false交接：几何、方向、状态和稳定identity采用独立plane。
/// 两个16B plane、一个8B plane、两个4B plane和独立identity；不交接材质纹素。
/// position/offset、normal、direction、distance 保留原 FP32；quad 任意朝向和非共面。
/// throughput=1、eta=1、medium=相机实际介质、depth/events=0 来自 K1 不消费顶点，
/// 非体素假设；材质纹素由叶子按键读取，不跨K1/K2交接。
[[vk::binding(10, 0)]]
RWStructuredBuffer<float4> landing_position;
[[vk::binding(11, 0)]]
RWStructuredBuffer<float4> landing_normal_direction_x;
[[vk::binding(12, 0)]]
RWStructuredBuffer<uint> landing_header;
[[vk::binding(13, 0)]]
RWStructuredBuffer<float2> landing_direction_yz;
[[vk::binding(14, 0)]]
RWStructuredBuffer<float> landing_distance;
[[vk::binding(15, 0)]]
RWStructuredBuffer<float4> prefix_image;

[[vk::binding(26, 0)]]
RWStructuredBuffer<uint4> landing_identity;

[shader("miss")]
void primary_miss(inout TracePayload payload)
{
    payload.primitive = 0xffffffffu;
}

[shader("anyhit")]
void primary_anyhit(inout TracePayload payload, BuiltInTriangleIntersectionAttributes attributes)
{
    if (!material_accepts(region_primitive(InstanceID(), PrimitiveIndex()), attributes.barycentrics,
                          HitKind() == 0xfeu))
        IgnoreHit();
}

[shader("closesthit")]
void primary_closesthit(inout TracePayload payload,
                        BuiltInTriangleIntersectionAttributes attributes)
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(primary_closesthit)
#endif
    payload.primitive = region_primitive(InstanceID(), PrimitiveIndex());
    payload.bary = attributes.barycentrics;
    payload.distance = RayTCurrent();
    float3 a = HitTriangleVertexPosition(0), b = HitTriangleVertexPosition(1),
           c = HitTriangleVertexPosition(2);
    payload.surface = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                               WorldToObject3x4());
    payload.shading_normal = 0;
    if (PRIMEPT_AUTHORED_NORMAL != 0)
        payload.shading_normal =
            mapped_shading_normal(payload.primitive, payload.bary, b - a, c - a,
                                  (float3x3)ObjectToWorld3x4(), payload.surface.normal);
}

void store_landing(uint pixel, pt::TransportCall data)
{
    uint flags = data.hit.capability | (uint(data.hit.front) << 8) | (data.path.status << 16);
    landing_header[pixel] = flags;
    if (data.path.status != pt::PATH_ACTIVE)
        return;
    landing_position[pixel] = float4(data.hit.surface.position, data.hit.surface.offset);
    landing_normal_direction_x[pixel] = float4(data.hit.surface.normal, data.path.direction.x);
    landing_direction_yz[pixel] = data.path.direction.yz;
    landing_distance[pixel] = data.hit.distance;
    landing_identity[pixel] = data.hit.identity;
}

pt::TransportCall load_landing(uint pixel, uint2 coordinate, uint flags)
{
    pt::TransportCall data = (pt::TransportCall)0;
    float4 position = landing_position[pixel];
    float4 normal_direction = landing_normal_direction_x[pixel];
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
    uint count = sample_counts[pixel];
    data.sample = parameters[0].rr_sample +
                  (count >= parameters[0].image.z ? count - parameters[0].image.z : 0u);
    return data;
}

[shader("raygeneration")]
void prefix_main()
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(prefix_main)
#endif
    Params params = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = coordinate.y * params.image.x + coordinate.x;
    pt::TransportCall data = (pt::TransportCall)0;
    data.pixel = coordinate;
    bool diagnostic = (params.output_size.z & 1u) != 0;
    uint count = sample_counts[pixel];
    data.sample = params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u);
    if (!diagnostic && accumulation[pixel].w < 0)
    {
        data.path.status =
            accumulation[pixel].w == -3 ? pt::PATH_DIELECTRIC_LIMIT : pt::PATH_INVALID;
        store_landing(pixel, data);
        prefix_image[pixel] = 0;
        return;
    }
    pt::PathSampler sampler =
        pt::path_sampler(coordinate, data.sample, params.options.z, params.options.x);
    float2 offset = params.reconstruction.w > 0 ? 0.5 + params.reconstruction.xy : sampler.film();
    float2 film = (float2(coordinate) + offset) / float2(params.image.xy);
    data.path.origin = params.origin.xyz;
    data.path.direction = geometry::camera_scaled_direction(params.forward.xyz, params.right.xyz,
                                                            params.up.xyz, film);
    data.path.throughput = 1;
    data.path.eta_scale = 1;
    data.path.medium = parameters[0].initial_medium;
    float distance;
    ModScene provider;
    if (params.reconstruction.w > 0)
    {
        pt::DenoiseCamera previous = { params.previous_position, params.previous_forward,
                                       params.previous_right, params.previous_up };
        pt::DenoiseCamera current = geometry::camera_from_scaled(
            params.origin.xyz, params.forward.xyz, params.right.xyz, params.up.xyz);
        Guides guides = { coordinate, film, current, previous };
        pt::trace_prefix(provider, data, settings(params), distance, guides);
    }
    else
        pt::trace_prefix(provider, data, settings(params), distance);
    store_landing(pixel, data);
    prefix_image[pixel] = float4(data.path.radiance, distance);
}

[shader("raygeneration")]
void render_main()
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_main)
#endif
    Params params = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = coordinate.y * params.image.x + coordinate.x;
    uint header = landing_header[pixel];
    uint status = (header >> 16) & 3u;
    if (status != pt::PATH_ACTIVE)
    {
        output_image[pixel] = float4(0, 0, 0,
                                     status == pt::PATH_INVALID            ? -1
                                     : status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                           : 0);
        return;
    }
    pt::TransportCall data = load_landing(pixel, coordinate, header);
    ModScene provider;
    pt::trace_tail(provider, data);
    output_image[pixel] =
        float4(data.path.radiance, data.path.status == pt::PATH_INVALID            ? -1
                                   : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                   : 0);
}

[shader("callable")]
void shade_opaque(inout ModTransportCall call)
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_opaque)
#endif
    pt::TransportCall data = call.unpack();
    ModScene provider;
    pt::process_flow<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, false>(provider, data,
                                                                        settings(parameters[0]));
    call.store<OPENPBR_FLOW>(data.path);
}

// 0=原调度，1=同raygen的callable对照，2=Hit Group/RayQuery，3=Hit Group/独立遍历。
#ifndef PRIMEPT_DISPATCH
#define PRIMEPT_DISPATCH 0
#endif

[shader("closesthit")]
void shade_hit(inout ModTransportCall call, BuiltInTriangleIntersectionAttributes attributes)
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_hit)
#endif
#ifdef PRIMEPT_HIT_STUB
    call.state = pt::PATH_DONE;
    call.point.position = 0;
    call.point.normal = 0;
    call.identity.xyz = 0;
    call.radiance = 1;
#else
    pt::TransportCall data = call.unpack();
    ModScene provider;
    pt::DirectWeights weights;
    pt::process_flow_vertex<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, false, true>(
        provider, data, settings(parameters[0]), weights);
    call.store<OPENPBR_FLOW>(data.path);
    // 输入已消费的normal/identity复用为24B灯权重，不扩大payload。
    call.point.normal = weights.sun;
    call.identity.xyz = asuint(weights.lamp);
#endif
}

[shader("raygeneration")]
void prefix_empty()
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(prefix_empty)
#endif
}

bool shadow_occluded(geometry::Ray connection)
{
    if (PRIMEPT_DISPATCH == 3)
    {
        RayDesc ray = { connection.origin, connection.t_min, connection.direction,
                        connection.t_max };
        uint unused = 0;
        HitObject shadow = HitObject::TraceRay(
            scene, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER,
            0xff, 0, 1, 0, ray, unused);
        return shadow.IsHit();
    }
    return ModScene().occluded(connection);
}

pt::Hit object_hit(HitObject object, float3 direction)
{
    BuiltInTriangleIntersectionAttributes attributes =
        object.GetAttributes<BuiltInTriangleIntersectionAttributes>();
    float3 vertices[3] = object.GetTriangleVertexPositions();
    // EXT返回object-space顶点，与position-fetch closest-hit采用相同误差界。
    pt::Hit hit = {};
    hit.surface = geometry::triangle_point(
        vertices[0], vertices[1], vertices[2], attributes.barycentrics,
        transpose(object.GetObjectToWorld()), transpose(object.GetWorldToObject()));
    uint primitive = region_primitive(object.GetInstanceID(), object.GetPrimitiveIndex());
    uint normal = mapped_shading_normal(
        primitive, attributes.barycentrics, vertices[1] - vertices[0], vertices[2] - vertices[0],
        (float3x3)transpose(object.GetObjectToWorld()), hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = object.GetRayDesc().TMax;
    hit.front = dot(direction, hit.surface.normal) < 0;
    return hit;
}

[shader("raygeneration")]
void render_fused()
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_fused)
#endif
    Params params = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = coordinate.y * params.image.x + coordinate.x;
    pt::TransportCall data = {};
    data.pixel = coordinate;
    uint count = sample_counts[pixel];
    bool diagnostic = (params.output_size.z & 1u) != 0;
    data.sample = params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u);
    if (!diagnostic && accumulation[pixel].w < 0)
    {
        output_image[pixel] = float4(0, 0, 0, accumulation[pixel].w);
        return;
    }
    pt::PathSampler sampler =
        pt::path_sampler(coordinate, data.sample, params.options.z, params.options.x);
    float2 offset = params.reconstruction.w > 0 ? 0.5 + params.reconstruction.xy : sampler.film();
    float2 film = (float2(coordinate) + offset) / float2(params.image.xy);
    data.path.origin = params.origin.xyz;
    data.path.direction = geometry::camera_scaled_direction(params.forward.xyz, params.right.xyz,
                                                            params.up.xyz, film);
    data.path.throughput = 1;
    data.path.eta_scale = 1;
    data.path.medium = params.initial_medium;
    ModScene provider;
    float distance = pt::FAR;
    while (data.path.status == pt::PATH_ACTIVE)
    {
        RayDesc ray = { data.path.origin, 0, data.path.direction, pt::FAR };
        uint unused = 0;
        HitObject found = HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
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
        data.path.throughput *= pt::segment_transmittance(data.path.medium.yzw, data.hit.distance);
        uint program = material_words.Load(material_id(data.hit.identity.x) * 112u + 64u) >> 2u;
        found.SetShaderTableIndex(program + 1u);
#ifdef PRIMEPT_SER
        ReorderThread(found, 0u, 0u);
#endif
        ModTransportCall call = ModTransportCall.input(data);
        HitObject::Invoke(found, call);
        // 只保活几何点和采样身份，灯样本不跨SER/材质阶段。
        pt::LightSample light;
        float3 direct = 0;
        if (any(call.point.normal > 0))
        {
            light = provider.sample_sun(data.hit.surface, sampler.sun(data.path.depth));
            if (!shadow_occluded(light.visibility))
                direct +=
                    call.point.normal * provider.light_emission(data.hit.surface, light, true);
        }
        float3 lamp = asfloat(call.identity.xyz);
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
