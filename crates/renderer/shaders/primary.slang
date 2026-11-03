/// 只负责硬件命中和 K1/K2 交接；路径推进完全使用引擎 pt.integrator。
module primary;
import mod_scene;
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

[[vk::binding(7, 0)]]
RWStructuredBuffer<float4> output_image;
[[vk::binding(8, 0)]]
RWStructuredBuffer<float4> accumulation;
[[vk::binding(9, 0)]]
RWStructuredBuffer<uint> sample_counts;

/// prefix=false交接：原60B几何/方向/RGB与新增16B identity独立plane。
/// 三个 16B plane、一个 8B plane、一个 4B plane 都有消费者；RGB/header 首读一次。
/// position/offset、normal、direction、distance 保留原 FP32；quad 任意朝向和非共面。
/// throughput=1、eta=1、medium=相机实际介质、depth/events=0 来自 K1 不消费顶点，
/// 非体素假设。未来增加 delta、介质或发光等能力须发布新 pipeline/资源 ABI。
[[vk::binding(10, 0)]]
RWStructuredBuffer<float4> landing_position;
[[vk::binding(11, 0)]]
RWStructuredBuffer<float4> landing_normal_direction_x;
[[vk::binding(12, 0)]]
RWStructuredBuffer<float4> landing_rgb_header;
[[vk::binding(13, 0)]]
RWStructuredBuffer<float2> landing_direction_yz;
[[vk::binding(14, 0)]]
RWStructuredBuffer<float> landing_distance;
[[vk::binding(15, 0)]]
RWStructuredBuffer<float4> prefix_image;

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
[[vk::binding(26, 0)]]
RWStructuredBuffer<uint4> landing_identity;

public struct Guides : pt::IPrefixGuides
{
    public uint2 coordinate;
    public float2 film;
    public pt::DenoiseCamera current;
    public pt::DenoiseCamera previous;

    public void surface(pt::TransportCall data)
    {
        ModScene provider;
        Params p = parameters[0];
        pt::GuideEndpoint endpoint = pt::trace_guide(provider, data, p.options.y);
        pt::GuideProjection projection = pt::project_guide(endpoint, current, previous, film,
                                                           p.image.xy, p.reconstruction.z > 0);
        pt::DenoiseGuides guides = pt::guide_albedos(provider.energy(), endpoint);
        guide_depth[coordinate] = projection.depth;
        guide_motion[coordinate] = projection.motion;
        guide_normal[coordinate] = float4(projection.normal, guides.normal_roughness.w);
        guide_diffuse[coordinate] = float4(guides.diffuse_albedo, 0);
        guide_specular[coordinate] = float4(guides.specular_albedo, 0);

        pt::GuideEndpoint reflection = pt::trace_reflection_guide(provider, data, p.options.y);
        pt::GuideProjection reflected = pt::project_guide(reflection, current, previous, film,
                                                          p.image.xy, p.reconstruction.z > 0);
        guide_distance[coordinate] =
            (reflection.flags & pt::GUIDE_SKY) != 0 ? 0 : reflection.distance;
        guide_specular_motion[coordinate] =
            (reflected.flags & pt::GUIDE_INVALID) != 0 ? projection.motion : reflected.motion;
    }

    public void environment(pt::TransportCall data)
    {
        guide_depth[coordinate] = pt::DENOISE_SKY_DEPTH;
        float2 pixels = 0;
        if (parameters[0].reconstruction.z > 0 &&
            (!pt::denoise_motion(previous, film, data.path.direction, true, parameters[0].image.xy,
                                 pixels) ||
             any(abs(pixels) > 65504)))
            pixels = 0;
        guide_motion[coordinate] = pixels;
        guide_normal[coordinate] = float4(0, 0, 0, 1);
        guide_diffuse[coordinate] = float4(0.5, 0.5, 0.5, 0);
        guide_specular[coordinate] = float4(0.5, 0.5, 0.5, 0);
        guide_distance[coordinate] = 0;
        guide_specular_motion[coordinate] = pixels;
    }
};

[shader("miss")]
void primary_miss(inout TracePayload payload)
{
    payload.primitive = 0xffffffffu;
}

[shader("anyhit")]
void primary_anyhit(inout TracePayload payload, BuiltInTriangleIntersectionAttributes attributes)
{
    if (!material_accepts(PrimitiveIndex() + InstanceID() * 2u, attributes.barycentrics,
                          HitKind() == 0xfeu))
        IgnoreHit();
}

[shader("closesthit")]
void primary_closesthit(inout TracePayload payload,
                        BuiltInTriangleIntersectionAttributes attributes)
{
    payload.primitive = PrimitiveIndex() + InstanceID() * 2u;
    payload.bary = attributes.barycentrics;
    payload.distance = RayTCurrent();
    float3 a = HitTriangleVertexPosition(0), b = HitTriangleVertexPosition(1),
           c = HitTriangleVertexPosition(2);
    payload.surface = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                               WorldToObject3x4());
    float3 edge1 = mul((float3x3)ObjectToWorld3x4(), b - a);
    float3 edge2 = mul((float3x3)ObjectToWorld3x4(), c - a);
    payload.shading_normal = mapped_shading_normal(payload.primitive, payload.bary, edge1, edge2,
                                                   payload.surface.normal);
}

void store_landing(uint pixel, pt::TransportCall data)
{
    uint flags = data.hit.capability | (uint(data.hit.front) << 8) | (data.path.status << 16);
    landing_rgb_header[pixel] = float4(data.hit.texel.rgb, asfloat(flags));
    if (data.path.status != pt::PATH_ACTIVE)
        return;
    landing_position[pixel] = float4(data.hit.surface.position, data.hit.surface.offset);
    landing_normal_direction_x[pixel] = float4(data.hit.surface.normal, data.path.direction.x);
    landing_direction_yz[pixel] = data.path.direction.yz;
    landing_distance[pixel] = data.hit.distance;
    landing_identity[pixel] = data.hit.identity;
}

pt::TransportCall load_landing(uint pixel, uint2 coordinate, float4 rgb_header)
{
    pt::TransportCall data = (pt::TransportCall)0;
    uint flags = asuint(rgb_header.w);
    float4 position = landing_position[pixel];
    float4 normal_direction = landing_normal_direction_x[pixel];
    data.hit.surface = { position.xyz, normal_direction.xyz, position.w };
    data.hit.capability = flags & 255u;
    data.hit.front = (flags & 256u) != 0;
    data.hit.texel = float4(rgb_header.xyz, 1);
    data.hit.distance = landing_distance[pixel];
    data.hit.identity = landing_identity[pixel];
    data.path.direction = float3(normal_direction.w, landing_direction_yz[pixel]);
    data.path.eta_scale = 1;
    data.path.throughput = 1;
    data.path.medium = parameters[0].initial_medium;
    data.pixel = coordinate;
    data.sample = parameters[0].reconstruction.w > 0
                      ? parameters[0].temporal.x + sample_counts[pixel]
                      : sample_counts[pixel];
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
    data.sample = params.reconstruction.w > 0 ? params.temporal.x + sample_counts[pixel]
                                              : sample_counts[pixel];
    if (accumulation[pixel].w < 0)
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
    float4 rgb_header = landing_rgb_header[pixel];
    uint status = (asuint(rgb_header.w) >> 16) & 3u;
    if (status != pt::PATH_ACTIVE)
    {
        output_image[pixel] = float4(0, 0, 0,
                                     status == pt::PATH_INVALID            ? -1
                                     : status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                           : 0);
        return;
    }
    pt::TransportCall data = load_landing(pixel, coordinate, rgb_header);
    ModScene provider;
    pt::trace_tail(provider, data);
    output_image[pixel] =
        float4(data.path.radiance, data.path.status == pt::PATH_INVALID            ? -1
                                   : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                   : 0);
}

[shader("callable")]
void shade_opaque(inout pt::TransportCall data)
{
    ModScene provider;
    pt::process_flow<OPENPBR_FLOW, ModScene, 3>(provider, data, settings(parameters[0]));
}
