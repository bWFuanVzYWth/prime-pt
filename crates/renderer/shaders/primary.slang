/// 只负责硬件命中和 K1/K2 交接；路径推进完全使用引擎 pt.integrator。
module primary;
import mod_scene;
import mod_call;
import mod_guides;
import params;
import pt.integrator;
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
    uint count = (parameters[0].output_size.z & 1u) != 0 ? 0u : sample_counts[pixel];
    data.sample = parameters[0].reconstruction.w > 0 || (parameters[0].output_size.z & 1u) != 0
                      ? parameters[0].rr_sample + count
                      : count;
    return data;
}

[shader("raygeneration")]
void prefix_main()
{
    Params params = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = coordinate.y * params.image.x + coordinate.x;
    pt::TransportCall data = (pt::TransportCall)0;
    data.pixel = coordinate;
    bool diagnostic = (params.output_size.z & 1u) != 0;
    uint count = diagnostic ? 0u : sample_counts[pixel];
    data.sample = params.reconstruction.w > 0 || diagnostic ? params.rr_sample + count : count;
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
    pt::TransportCall data = call.unpack();
    ModScene provider;
    pt::process_flow<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, false>(provider, data,
                                                                        settings(parameters[0]));
    call.store<OPENPBR_FLOW>(data.path);
}
