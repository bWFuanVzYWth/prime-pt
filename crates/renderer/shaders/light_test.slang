/// 无窗口正逆PDF探针；生产采样、静态纹理alias和动态非共面面积共用同一入口。
module light_test;
import mod_scene;
import mod_lights;
import params;
import pt.scene;
import pt.light_sampling;
import geometry.ray;

/// 每lane输出[forward_pdf,reverse_pdf,真实发光BT.709亮度,有效样本]；仅测试额外binding。
[[vk::binding(37, 0)]]
RWStructuredBuffer<float4> probes;

[shader("compute")]
[numthreads(1, 1, 1)]
void region_primitive_probe_main()
{
    probes[0] = asfloat(uint4(region_primitive(0u, 0u), region_primitive(0u, 1u),
                              region_primitive(1u, 0u), region_primitive(1u, 3u)));
}

[shader("compute")]
[numthreads(32, 1, 1)]
void light_probe_main(uint3 id: SV_DispatchThreadID)
{
    if (id.x >= 32u)
        return;
    geometry::SurfacePoint point = { float3(2, 0, 4), float3(0, 0, -1), 1.0e-5 };
    // 固定开区间格点覆盖alias列与片内warp，不读取帧随机状态。
    float lane = float(id.x) + 0.5;
    float3 random = float3(lane / 32.0, frac(lane * 0.61803398875), frac(lane * 0.75487766625));
    Params p = parameters[0];
    pt::LightSample sample = sample_quad_light(point, random, p);
    if (!(sample.pdf > 0))
    {
        probes[id.x] = 0;
        return;
    }
    uint index = sample.emission_reference;
    uint flags = asuint(sample.emission.z);
    pt::QuadAreaSample area = { flags & 1u, sample.emission.xy, 0 };
    uint start = index * 3u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(
        light_vertices[start], light_vertices[start + 1u], light_vertices[start + 2u]);
    float3 translation = float3(light_regions[index].xyz - p.anchor_region.xyz) * 64.0;
    float3x4 transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    float3x4 inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    geometry::TriangleEndpoint endpoint = pt::quad_endpoint(vertices, area, transform, inverse);
    pt::Hit hit = {};
    hit.surface = { endpoint.position, endpoint.normal, endpoint.base_offset };
    hit.identity = uint4(light_meta[index].w * 2u + area.half, asuint(area.bary), 0);
    hit.front = (flags & 2u) != 0;
    hit.distance = sample.distance;
    float reverse = quad_emission_pdf(hit, point.position);
    float3 emission = quad_light_emission(sample, p.block_emission_scale);
    float luminance = dot(emission, float3(0.2126, 0.7152, 0.0722));
    bool valid = isfinite(sample.pdf) && reverse > 0 && isfinite(reverse) &&
                 all(isfinite(emission)) && luminance > 0;
    probes[id.x] = float4(sample.pdf, reverse, luminance, float(valid));
}
