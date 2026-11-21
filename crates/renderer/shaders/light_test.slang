/// 无窗口正逆PDF探针；生产采样、静态纹理alias和动态非共面面积共用同一入口。
module light_test;
import mod_scene;
import mod_lights;
import world_features;
import params;
import pt.scene;
import geometry.ray;
import light_sampling.area;
import light_sampling.sample;

/// 每lane输出[forward_pdf,reverse_pdf,真实发光BT.709亮度,有效域（1 静态、2
/// 动态）]；仅测试额外binding。
[[vk::binding(39, 0)]]
var probes : RWStructuredBuffer<float4>;

[shader("compute")]
[numthreads(1, 1, 1)]
func region_primitive_probe_main()->void
{
    probes[0] = asfloat(uint4(region_primitive(0u, 0u), region_primitive(0u, 1u),
                              region_primitive(1u, 0u), region_primitive(1u, 3u)));
}

[shader("compute")]
[numthreads(2, 1, 1)]
func triangle_identity_probe_main(lane: uint3: SV_DispatchThreadID)->void
{
    let xy : float2 = lane.x == 0 ? float2(0.25, 0.75) : float2(0.75, 0.25);
    let ray : RayDesc = { float3(xy, 2), 0.0, float3(0, 0, -1), 4.0 };
    var query : RayQuery<RAY_FLAG_FORCE_OPAQUE>;
    query.TraceRayInline(scene, RAY_FLAG_NONE, 0xff, ray);
    while (query.Proceed())
    {
    }
    var value : uint4 = 0;
    if (query.CommittedStatus() == COMMITTED_TRIANGLE_HIT)
    {
        let primitive
            : uint = region_primitive(query.CommittedInstanceID(), query.CommittedPrimitiveIndex());
        value = uint4(stable_triangle(primitive), 1u);
    }
    probes[lane.x] = asfloat(value);
}

[shader("compute")]
[numthreads(32, 1, 1)]
func light_probe_main(id: uint3: SV_DispatchThreadID)->void
{
    var count : uint;
    var stride : uint;
    probes.GetDimensions(count, stride);
    if (id.x >= count)
        return;
    let surface_point : geometry::SurfacePoint = { float3(2, 0, 4), float3(0, 0, -1), 1.0e-5 };
    // 固定开区间格点覆盖alias列与片内warp，不读取帧随机状态。
    let lane = float(id.x) + 0.5;
    let random =
        float3(lane / float(count), frac(lane * 0.61803398875), frac(lane * 0.75487766625));
    let p : Params = parameters[0];
    let light_sample : light_sampling::LightSample = sample_quad_light(surface_point, random, p);
    if (!(light_sample.pdf > 0))
    {
        probes[id.x] = 0;
        return;
    }
    if (count > 32u)
    {
        let emitted = quad_light_emission(light_sample, p.block_emission_scale);
        let estimate = dot(emitted, float3(0.2126, 0.7152, 0.0722)) *
                       max(-light_sample.direction.z, 0.0) / light_sample.pdf;
        let moving = (light_sample.emission_reference & world_features::primitive_bit) != 0;
        probes[id.x] =
            float4(estimate, float(moving && light_sample.distance < 4.0), float(moving), 1);
        return;
    }
    let index : uint = light_sample.emission_reference;
    let flags : uint = asuint(light_sample.emission.z);
    let area : light_sampling::QuadAreaSample = { flags & 1u, light_sample.emission.xy, 0 };
    let dynamic = (index & world_features::primitive_bit) != 0;
    let slot = index & ~world_features::primitive_bit;
    var vertices : light_sampling::LightQuadVertices;
    var translation = float3(0);
    var primitive : uint;
    if (dynamic)
    {
        vertices = { world_features::faces[slot].position[0].xyz,
                     world_features::faces[slot].position[1].xyz,
                     world_features::faces[slot].position[2].xyz,
                     world_features::faces[slot].position[3].xyz };
        primitive = world_features::primitive_bit | (slot * 2u + area.half);
    }
    else
    {
        vertices = emitter_vertices(index);
        translation = float3(light_regions[index].xyz - p.anchor_region.xyz) * 64.0;
        primitive = light_meta[index].w * 2u + area.half;
    }
    let transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    let inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    let endpoint : geometry::TriangleEndpoint =
                       light_sampling::quad_endpoint(vertices, area, transform, inverse);
    var hit : pt::Hit = {};
    hit.surface = { endpoint.position, endpoint.normal, endpoint.base_offset };
    hit.identity = uint4(primitive, asuint(area.bary), 0);
    hit.front = (flags & 2u) != 0;
    hit.distance = light_sample.distance;
    let reverse : float = quad_emission_pdf<PublishedArea>(hit, surface_point.position,
                                                           p.anchor_region.xyz);
    let emission : float3 = quad_light_emission(light_sample, p.block_emission_scale);
    let luminance : float = dot(emission, float3(0.2126, 0.7152, 0.0722));
    let valid : bool = isfinite(light_sample.pdf) && reverse > 0 && isfinite(reverse) &&
                       all(isfinite(emission)) && luminance > 0;
    probes[id.x] =
        float4(light_sample.pdf, reverse, luminance, valid ? (dynamic ? 2.0 : 1.0) : 0.0);
}
