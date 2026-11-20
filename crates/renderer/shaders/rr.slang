/// RR 在场景线性 HDR 中重建；曝光、DRT 和宿主上下翻转只在呈现执行。
module rr;
import params;
import pt.guide_output;

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
