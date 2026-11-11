/// K3 复用引擎线性累积；宿主编码与上下翻转在独立呈现阶段处理。
module post;
import params;
import pt.post;
import pt.scene;
import pt.path_sampler;
import atmosphere.sampling;
import color.primaries;

[[vk::binding(0, 0)]]
StructuredBuffer<Params> parameters;
[[vk::binding(1, 0)]]
RWStructuredBuffer<float4> output_image;
[[vk::binding(2, 0)]]
RWStructuredBuffer<float4> accumulation;
[[vk::binding(3, 0)]]
RWStructuredBuffer<uint> sample_counts;
[[vk::binding(4, 0)]]
StructuredBuffer<float4> prefix_image;
[[vk::binding(5, 0)]]
Texture3D<float4> aerial_light;
[[vk::binding(6, 0)]]
Texture3D<float4> aerial_transmittance;
[[vk::binding(7, 0)]]
SamplerState linear_sampler;

[shader("compute")]
[numthreads(8, 8, 1)]
void post_main(uint3 id: SV_DispatchThreadID)
{
    Params params = parameters[0];
    if (any(id.xy >= params.image.xy))
        return;
    uint pixel = id.y * params.image.x + id.x;
    bool diagnostic = (params.output_size.z & 1u) != 0;
    float4 sum = accumulation[pixel];
    float4 tail = output_image[pixel];
    uint count = sample_counts[pixel];
    float4 prefix = prefix_image[pixel];
    float3 radiance = prefix.xyz + tail.xyz;
    if (tail.w >= 0 && prefix.w < pt::FAR && params.lighting.w > 0 &&
        all(params.initial_medium == float4(1, 0, 0, 0)))
    {
        pt::PathSampler sampler = pt::path_sampler(
            id.xy, params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u),
            params.options.z, params.options.x);
        float2 offset = params.reconstruction.w > 0 || (params.output_size.z & 2u) != 0
                            ? 0.5 + params.reconstruction.xy
                            : sampler.film();
        float2 film = (float2(id.xy) + offset) / float2(params.image.xy);
        atmosphere::AerialSample aerial =
            atmosphere::aerial_sample(aerial_light, aerial_transmittance, linear_sampler, film,
                                      prefix.w * 0.001, params.atmosphere_settings.w);
        radiance = color::rec2020_to_bt709(
            atmosphere::composite_aerial(aerial, color::bt709_to_rec2020(radiance)));
        if ((params.output_size.z & 8u) == 0)
            radiance = max(radiance, 0);
    }
    uint paths = (params.output_size.z & 32u) != 0 ? params.image.w : 1u;
    // 本次均值代表1或2条输运路径，累积计数仍按实际路径计。
    for (uint lane = 0; lane < paths; lane++)
        pt::accumulate_sample(sum, count, radiance, tail.w);
    accumulation[pixel] = sum;
    sample_counts[pixel] = count;
    if (sum.w < 0 || count == params.image.z + params.image.w)
        output_image[pixel] = pt::resolve_image(sum, count, true, params.drt);
}
