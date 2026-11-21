/// K3 复用引擎线性累积；宿主编码与上下翻转在独立呈现阶段处理。
module post;
import params;
import pt.accumulation;
import pt.scene;
import pt.path_sampler;
import atmosphere.terrain_composite;
import atmosphere.terrain_frame;
import camera.pinhole;

[[vk::binding(0, 0)]]
var parameters : StructuredBuffer<Params>;
[[vk::binding(1, 0)]]
var output_image : RWStructuredBuffer<float4>;
[[vk::binding(2, 0)]]
var accumulation : RWStructuredBuffer<float4>;
[[vk::binding(3, 0)]]
var sample_counts : RWStructuredBuffer<uint>;
[[vk::binding(4, 0)]]
var prefix_image : StructuredBuffer<float4>;
[[vk::binding(5, 0)]]
var aerial_light : Texture3D<float4>;
[[vk::binding(6, 0)]]
var aerial_transmittance : Texture3D<float4>;
[[vk::binding(7, 0)]]
var linear_sampler : SamplerState;
[[vk::binding(8, 0)]]
var terrain_frames : StructuredBuffer<atmosphere::TerrainFrame>;
[[vk::binding(9, 0)]]
var terrain_radiance : Texture3D<float4>;
[[vk::binding(10, 0)]]
var first_hits : StructuredBuffer<uint>;

func post_pixel<let paths : uint, let composite : uint, let frame_film : bool,
                let denoised : bool = false>(id: uint3)
    ->void
{
    let params : Params = parameters[0];
    if (any(id.xy >= params.image.xy))
        return;
    let pixel : uint = id.y * params.image.x + id.x;
    if (!denoised && params.reconstruction.w == 3)
    {
        if ((params.output_size.z & 32u) == 0)
            sample_counts[pixel]++;
        return;
    }
    let frame_local = (params.output_size.z & 64u) != 0;
    var sum = float4(0);
    var count = 0u;
    if (!frame_local && !denoised)
    {
        sum = accumulation[pixel];
        count = sample_counts[pixel];
    }
    let tail : float4 = output_image[pixel];
    let stored = prefix_image[pixel];
    let prefix = frame_local && params.reconstruction.w == 2 ? float4(0, 0, 0, stored.w) : stored;
    // NRD 的 prefix 只含当前像素固定光路上的自发光，和已滤波照明一起进入相机段大气。
    var radiance : float3 = prefix.xyz + tail.xyz;
    if (composite != 0 && tail.w >= 0 &&
        (!denoised || (params.lighting.w > 0 && all(params.initial_medium == float4(1, 0, 0, 0)))))
    {
        let sampler = pt::path_sampler(
            id.xy, params.rr_sample + (count >= params.image.z ? count - params.image.z : 0u),
            params.options.z, params.options.x);
        let offset : float2 = frame_film ? 0.5 + params.reconstruction.xy : sampler.film();
        let film = camera::camera_sample_uv_offset(id.xy, params.inverse_extent, offset);
        if (composite == 1)
            radiance = atmosphere::composite_clear_aerial_bt709(
                aerial_light, aerial_transmittance, linear_sampler, film, prefix.w,
                params.atmosphere_settings.w, prefix.w < pt::FAR, radiance);
        else if (composite == 2)
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
    if (denoised)
    {
        output_image[pixel] = float4(radiance, tail.w);
        return;
    }
    // 本次均值代表1或2条输运路径，累积计数仍按实际路径计。
    for (var lane: uint = 0; lane < paths; lane++)
        pt::accumulate_sample(sum, count, radiance, tail.w);
    if (!frame_local)
    {
        accumulation[pixel] = sum;
        sample_counts[pixel] = count;
    }
    if (frame_local || sum.w < 0 || count == params.image.z + params.image.w)
        output_image[pixel] = pt::resolve_linear_hdr(sum, count);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_nrd(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<2, 3, true, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_plain_1(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<1, 0, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_plain_2(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<2, 0, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_random_1(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<1, 1, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_random_2(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<2, 1, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_jittered_1(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<1, 1, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_clear_jittered_2(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<2, 1, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_random_1(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<1, 2, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_random_2(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<2, 2, false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_jittered_1(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<1, 2, true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func post_shadowed_jittered_2(id: uint3: SV_DispatchThreadID)->void
{
    post_pixel<2, 2, true>(id);
}

// 显式参考入口；生产按FramePlan选择上面的固定叶子。
[shader("compute")]
[numthreads(8, 8, 1)]
func post_main(id: uint3: SV_DispatchThreadID)->void
{
    let p = parameters[0];
    let composite = p.lighting.w > 0 && all(p.initial_medium == float4(1, 0, 0, 0));
    let frame_film = p.reconstruction.w > 0;
    let realtime = (p.output_size.z & 32u) != 0;
    if (!composite)
    {
        if (realtime)
            post_pixel<2, 0, false>(id);
        else
            post_pixel<1, 0, false>(id);
    }
    else if (frame_film)
    {
        if (realtime)
            post_pixel<2, 3, true>(id);
        else
            post_pixel<1, 3, true>(id);
    }
    else
    {
        if (realtime)
            post_pixel<2, 3, false>(id);
        else
            post_pixel<1, 3, false>(id);
    }
}
