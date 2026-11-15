/// K3 复用引擎线性累积；宿主编码与上下翻转在独立呈现阶段处理。
module post;
import params;
import pt.post;
import pt.scene;
import pt.path_sampler;
import atmosphere.terrain_composite;
import atmosphere.terrain_frame;

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
[[vk::binding(8, 0)]]
StructuredBuffer<atmosphere::TerrainFrame> terrain_frames;
[[vk::binding(9, 0)]]
Texture3D<float4> terrain_radiance;
[[vk::binding(10, 0)]]
StructuredBuffer<uint> first_hits;

func post_pixel<let Paths : uint, let Composite : uint, let FrameFilm : bool>(id: uint3)->void
{
    Params params = parameters[0];
    if (any(id.xy >= params.image.xy))
        return;
    uint pixel = id.y * params.image.x + id.x;
    float4 sum = accumulation[pixel];
    float4 tail = output_image[pixel];
    uint count = sample_counts[pixel];
    float4 prefix = prefix_image[pixel];
    float3 radiance = prefix.xyz + tail.xyz;
    if (Composite != 0 && tail.w >= 0)
    {
        pt::PathSampler sampler = pt::path_sampler(
            id.xy, params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u),
            params.options.z, params.options.x);
        float2 offset = FrameFilm ? 0.5 + params.reconstruction.xy : sampler.film();
        float2 film = (float2(id.xy) + offset) / float2(params.image.xy);
        if (Composite == 1)
            radiance = atmosphere::composite_clear_aerial_bt709(
                aerial_light, aerial_transmittance, linear_sampler, film, prefix.w,
                params.atmosphere_settings.w, prefix.w < pt::FAR, radiance);
        else if (Composite == 2)
            radiance = atmosphere::composite_shadowed_terrain_bt709(
                aerial_light, aerial_transmittance, terrain_radiance, first_hits, linear_sampler,
                terrain_frames[0], film, prefix.w, params.atmosphere_settings.w, prefix.w < pt::FAR,
                radiance);
        else
            radiance = atmosphere::composite_terrain_bt709(
                aerial_light, aerial_transmittance, terrain_radiance, first_hits, linear_sampler,
                terrain_frames[0], film, prefix.w, params.atmosphere_settings.w, prefix.w < pt::FAR,
                radiance);
        radiance = max(radiance, 0);
    }
    // 本次均值代表1或2条输运路径，累积计数仍按实际路径计。
    for (uint lane = 0; lane < Paths; lane++)
        pt::accumulate_sample(sum, count, radiance, tail.w);
    accumulation[pixel] = sum;
    sample_counts[pixel] = count;
    if (sum.w < 0 || count == params.image.z + params.image.w)
        output_image[pixel] = pt::resolve_linear_hdr(sum, count);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_plain_1(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<1, 0, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_plain_2(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<2, 0, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_random_1(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<1, 1, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_random_2(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<2, 1, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_jittered_1(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<1, 1, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_jittered_2(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<2, 1, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_random_1(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<1, 2, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_random_2(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<2, 2, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_jittered_1(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<1, 2, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_jittered_2(uint3 id: SV_DispatchThreadID)->void
{
    post_pixel<2, 2, true>(id);
}

// 显式参考入口；生产按FramePlan选择上面的固定叶子。
[shader("compute")]
[numthreads(8, 8, 1)]
func post_main(uint3 id: SV_DispatchThreadID)->void
{
    let p = parameters[0];
    let composite = p.lighting.w > 0 && all(p.initial_medium == float4(1, 0, 0, 0));
    let frameFilm = p.reconstruction.w > 0;
    let paths = (p.output_size.z & 32u) != 0 ? p.image.w : 1u;
    if (!composite)
    {
        if (paths == 2)
            post_pixel<2, 0, false>(id);
        else
            post_pixel<1, 0, false>(id);
    }
    else if (frameFilm)
    {
        if (paths == 2)
            post_pixel<2, 3, true>(id);
        else
            post_pixel<1, 3, true>(id);
    }
    else
    {
        if (paths == 2)
            post_pixel<2, 3, false>(id);
        else
            post_pixel<1, 3, false>(id);
    }
}
