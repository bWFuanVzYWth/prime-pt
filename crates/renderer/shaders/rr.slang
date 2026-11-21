/// RR 在场景线性 HDR 中重建；曝光、DRT 和宿主上下翻转只在呈现执行。
module rr;
import params;
import pt.guide_output;
import pt.rr_optics;

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
var optical_even : RWStructuredBuffer<uint2>;
[[vk::binding(6, 0)]]
var optical_odd : RWStructuredBuffer<uint2>;
[[vk::binding(7, 0)]]
[[vk::image_format("rg32f")]]
var motion : RWTexture2D<float2>;
[[vk::binding(8, 0)]]
[[vk::image_format("rg32f")]]
var specular_motion : RWTexture2D<float2>;
[[vk::binding(9, 0)]]
[[vk::image_format("r8")]]
var disocclusion : RWTexture2D<float>;
[shader("compute")]
[numthreads(8, 8, 1)]
func pack_main(id: uint3: SV_DispatchThreadID)->void
{
    let p : Params = parameters[0];
    if (any(id.xy >= p.image.xy))
        return;
    let hdr : float4 = pixels[id.y * p.image.x + id.x];
    let inputs : pt::RrInputs = { noisy,  optical_even,    optical_odd,
                                  motion, specular_motion, disocclusion };
    inputs.store(hdr.xyz, id.xy, p.image.xy, p.output_size.w,
                 p.reconstruction.xy - float2(p.previous_right.w, p.previous_up.w),
                 p.reconstruction.z > 0);
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

/// RR 在宿主已编码 PT 后失败时呈现本帧，下一帧切换到原生 NRD。
[shader("compute")]
[numthreads(8, 8, 1)]
func resolve_scaled(id: uint3: SV_DispatchThreadID)->void
{
    let p = parameters[0];
    if (any(id.xy >= p.output_size.xy))
        return;
    let src = min(uint2((float2(id.xy) + 0.5) * asfloat(p.programs.xy)), p.image.xy - 1u);
    linear_hdr[id.xy] = pixels[src.y * p.image.x + src.x];
}
