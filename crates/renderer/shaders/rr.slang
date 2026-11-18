/// RR 在场景线性 HDR 中重建；曝光、DRT 和宿主上下翻转只在呈现执行。
module rr;
import params;
import display.prime_drt;
import display.exposure;
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
[[vk::image_format("rgba8")]]
var target : RWTexture2D<float4>;
[[vk::binding(5, 0)]]
var guide_depth : Texture2D<float>;
[[vk::binding(6, 0)]]
var guide_normal : Texture2D<float4>;
[[vk::binding(7, 0)]]
var guide_diffuse : Texture2D<float4>;
[[vk::binding(8, 0)]]
var guide_specular : Texture2D<float4>;
[[vk::binding(9, 0)]]
var exposure_state : StructuredBuffer<display::ExposureState>;
[[vk::binding(10, 0)]]
var exposure_parameters : StructuredBuffer<uint4>;

func display_parameters<let automatic : bool>()->display::PrimeDrtParameters
{
    var drt : display::PrimeDrtParameters = parameters[0].drt;
    if (automatic)
        drt.exposure_multiplier *= exp2(exposure_state[0].exposure_ev);
    return drt;
}

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

func present_pixel<let automatic : bool>(id: uint3)->void
{
    let p : Params = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    let hdr : float3 = reconstructed.Load(int3(id.xy, 0)).xyz;
    let source : uint2 = min(id.xy * p.image.xy / p.output_size.xy, p.image.xy - 1);
    let invalid : bool = pixels[source.y * p.image.x + source.x].w < 0;
    target[uint2(id.x, p.output_size.y - 1 - id.y)] =
        invalid ? float4(1, 0, 1, 1)
                : float4(display::prime_drt_bt709(hdr, display_parameters<automatic>()), 1);
}

/// 本帧求值安全失败时以已有输入和 guides 执行边界保护空间重建，再作显示变换。
func fallback_pixel<let automatic : bool>(id: uint3)->void
{
    let p : Params = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    let source = fma(float2(id.xy) + 0.5, asfloat(p.programs.xy), -0.5);
    var valid : bool;
    let hdr : float3 = denoise::spatial_reconstruct(SpatialInput(), source, p.image.xy, valid);
    target[uint2(id.x, p.output_size.y - 1 - id.y)] =
        !valid ? float4(1, 0, 1, 1)
               : float4(display::prime_drt_bt709(hdr, display_parameters<automatic>()), 1);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func present_manual(id: uint3: SV_DispatchThreadID)->void
{
    present_pixel<false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func present_automatic(id: uint3: SV_DispatchThreadID)->void
{
    present_pixel<true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func present_main(id: uint3: SV_DispatchThreadID)->void
{
    if (asfloat(exposure_parameters[1].y) > 0)
        present_pixel<true>(id);
    else
        present_pixel<false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func fallback_manual(id: uint3: SV_DispatchThreadID)->void
{
    fallback_pixel<false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func fallback_automatic(id: uint3: SV_DispatchThreadID)->void
{
    fallback_pixel<true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func fallback_main(id: uint3: SV_DispatchThreadID)->void
{
    if (asfloat(exposure_parameters[1].y) > 0)
        fallback_pixel<true>(id);
    else
        fallback_pixel<false>(id);
}
