/// 左上原点线性 HDR 到宿主 RGBA8_UNORM；第 0 行对应宿主画面底部。
module present;
import params;
import display.prime_drt;
import display.exposure;

[[vk::binding(0, 0)]]
StructuredBuffer<Params> parameters;
[[vk::binding(1, 0)]]
StructuredBuffer<float4> pixels;
[[vk::binding(2, 0)]]
[[vk::image_format("rgba8")]]
RWTexture2D<float4> output_image;
[[vk::binding(3, 0)]]
StructuredBuffer<display::ExposureState> exposure_state;
[[vk::binding(4, 0)]]
StructuredBuffer<uint4> exposure_parameters;

func present_pixel<let Automatic : bool>(id: uint3)->void
{
    Params params = parameters[0];
    if (any(id.xy >= params.image.xy))
        return;
    float4 hdr = pixels[id.y * params.image.x + id.x];
    display::PrimeDrtParameters drt = params.drt;
    if (Automatic)
        drt.exposure_multiplier *= exp2(exposure_state[0].exposure_ev);
    uint2 target = uint2(id.x, params.image.y - 1 - id.y);
    output_image[target] =
        hdr.w < 0 ? float4(1, 0, 1, 1) : float4(display::prime_drt_bt709(hdr.xyz, drt), 1);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func present_manual(uint3 id: SV_DispatchThreadID)->void
{
    present_pixel<false>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func present_automatic(uint3 id: SV_DispatchThreadID)->void
{
    present_pixel<true>(id);
}

[shader("compute")]
[numthreads(8, 8, 1)]
func present_main(uint3 id: SV_DispatchThreadID)->void
{
    if (asfloat(exposure_parameters[1].y) > 0)
        present_pixel<true>(id);
    else
        present_pixel<false>(id);
}
