/// RR 在场景线性 HDR 中重建；曝光、DRT 和宿主上下翻转只在呈现执行。
module rr;
import params;
import display.prime_drt;
import display.exposure;
import pt.spatial_denoise;
import pt.denoise;

[[vk::binding(0, 0)]]
StructuredBuffer<Params> parameters;
[[vk::binding(1, 0)]]
StructuredBuffer<float4> pixels;
[[vk::binding(2, 0)]]
[[vk::image_format("rgba16f")]]
RWTexture2D<float4> noisy;
[[vk::binding(3, 0)]]
Texture2D<float4> reconstructed;
[[vk::binding(4, 0)]]
[[vk::image_format("rgba8")]]
RWTexture2D<float4> target;
[[vk::binding(5, 0)]]
Texture2D<float> guide_depth;
[[vk::binding(6, 0)]]
Texture2D<float4> guide_normal;
[[vk::binding(7, 0)]]
Texture2D<float4> guide_diffuse;
[[vk::binding(8, 0)]]
Texture2D<float4> guide_specular;
[[vk::binding(9, 0)]]
StructuredBuffer<display::ExposureState> exposure_state;
[[vk::binding(10, 0)]]
StructuredBuffer<uint4> exposure_parameters;

display::PrimeDrtParameters display_parameters()
{
    display::PrimeDrtParameters drt = parameters[0].drt;
    if (asfloat(exposure_parameters[1].y) > 0)
        drt.exposure_multiplier *= exp2(exposure_state[0].exposure_ev);
    return drt;
}

public struct SpatialInput : pt::ISpatialImage
{
    public pt::SpatialSample load(uint2 pixel)
    {
        float4 hdr = pixels[pixel.y * parameters[0].image.x + pixel.x];
        float depth = guide_depth.Load(int3(pixel, 0));
        return { hdr.xyz,
                 depth,
                 guide_normal.Load(int3(pixel, 0)),
                 guide_diffuse.Load(int3(pixel, 0)).xyz,
                 guide_specular.Load(int3(pixel, 0)).xyz,
                 depth != pt::DENOISE_SKY_DEPTH,
                 hdr.w >= 0 };
    }
};

[shader("compute")]
[numthreads(8, 8, 1)]
void pack_main(uint3 id: SV_DispatchThreadID)
{
    Params p = parameters[0];
    if (any(id.xy >= p.image.xy))
        return;
    float4 hdr = pixels[id.y * p.image.x + id.x];
    noisy[id.xy] = float4(clamp(hdr.xyz, 0, 65504), 1);
}

[shader("compute")]
[numthreads(8, 8, 1)]
void present_main(uint3 id: SV_DispatchThreadID)
{
    Params p = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    float3 hdr = reconstructed.Load(int3(id.xy, 0)).xyz;
    uint2 source = min(id.xy * p.image.xy / p.output_size.xy, p.image.xy - 1);
    bool invalid = pixels[source.y * p.image.x + source.x].w < 0;
    target[uint2(id.x, p.output_size.y - 1 - id.y)] =
        invalid ? float4(1, 0, 1, 1)
                : float4(display::prime_drt_bt709(hdr, display_parameters()), 1);
}

/// 本帧求值安全失败时以已有输入和 guides 执行边界保护空间重建，再作显示变换。
[shader("compute")]
[numthreads(8, 8, 1)]
void fallback_main(uint3 id: SV_DispatchThreadID)
{
    Params p = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    float2 source = (float2(id.xy) + 0.5) * float2(p.image.xy) / float2(p.output_size.xy) - 0.5;
    bool valid;
    float3 hdr = pt::spatial_reconstruct(SpatialInput(), source, p.image.xy, valid);
    target[uint2(id.x, p.output_size.y - 1 - id.y)] =
        !valid ? float4(1, 0, 1, 1)
               : float4(display::prime_drt_bt709(hdr, display_parameters()), 1);
}
