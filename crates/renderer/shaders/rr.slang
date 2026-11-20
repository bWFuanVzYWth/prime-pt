/// RR 在场景线性 HDR 中重建；曝光、DRT 和宿主上下翻转只在呈现执行。
module rr;
import params;
import pt.guide_output;
import denoise.spatial;

[[vk::binding(0, 0)]]
var parameters : StructuredBuffer<Params>;
[[vk::binding(1, 0)]]
var pixels : StructuredBuffer<float4>;
[[vk::binding(2, 0)]]
[[vk::image_format("rgba16f")]]
var noisy : RWTexture2D<float4>;
[[vk::binding(3, 0)]]
var reconstructed : Texture2D<float4>;
[[vk::binding(4, 0)]]
[[vk::image_format("rgba32f")]]
var linear_hdr : RWTexture2D<float4>;
[[vk::binding(5, 0)]]
var guide_depth : Texture2D<float>;
[[vk::binding(6, 0)]]
var guide_normal : Texture2D<float4>;
[[vk::binding(7, 0)]]
var guide_diffuse : Texture2D<float4>;
[[vk::binding(8, 0)]]
var guide_specular : Texture2D<float4>;
public struct SpatialInput : denoise::ISpatialImage
{
    public func load(pixel: uint2)->denoise::SpatialSample
    {
        let hdr : float4 = pixels[pixel.y * parameters[0].image.x + pixel.x];
        let depth : float = guide_depth.Load(int3(pixel, 0));
        let albedo : float4 = guide_diffuse.Load(int3(pixel, 0));
        let flags = uint(round(albedo.w * 255));
        return { hdr.xyz,
                 depth,
                 guide_normal.Load(int3(pixel, 0)),
                 albedo.xyz,
                 guide_specular.Load(int3(pixel, 0)).xyz,
                 (flags & pt::GUIDE_FOREGROUND) != 0,
                 hdr.w >= 0 && (flags & pt::GUIDE_INVALID) == 0 };
    }
};

[shader("compute")]
[numthreads(8, 8, 1)]
func pack_main(id: uint3: SV_DispatchThreadID)->void
{
    let p : Params = parameters[0];
    if (any(id.xy >= p.image.xy))
        return;
    let hdr : float4 = pixels[id.y * p.image.x + id.x];
    noisy[id.xy] = pt::reconstruction_input(hdr.xyz);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func resolve_raw(id: uint3: SV_DispatchThreadID)->void
{
    let p = parameters[0];
    if (all(id.xy < p.image.xy))
        linear_hdr[id.xy] = pixels[id.y * p.image.x + id.x];
}

[shader("compute")]
[numthreads(8, 8, 1)]
func resolve_rr(id: uint3: SV_DispatchThreadID)->void
{
    let p = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    let source = min(id.xy * p.image.xy / p.output_size.xy, p.image.xy - 1);
    linear_hdr[id.xy] =
        float4(reconstructed.Load(int3(id.xy, 0)).xyz, pixels[source.y * p.image.x + source.x].w);
}

/// RR 本帧求值安全失败时，导引保护重建边界；无效标记随 HDR 进入呈现。
[shader("compute")]
[numthreads(8, 8, 1)]
func resolve_spatial(id: uint3: SV_DispatchThreadID)->void
{
    let p = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    let source = fma(float2(id.xy) + 0.5, asfloat(p.programs.xy), -0.5);
    var valid : bool;
    let hdr = denoise::spatial_reconstruct(SpatialInput(), source, p.image.xy, valid);
    linear_hdr[id.xy] = float4(hdr, valid ? 1.0 : -1.0);
}
