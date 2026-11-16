/// 左上原点线性 HDR 到宿主 RGBA8_UNORM；第 0 行对应宿主画面底部。
module present;
import params;
import display.prime_drt;
import display.exposure;

[[vk::binding(0, 0)]]
var parameters : StructuredBuffer<Params>;
[[vk::binding(1, 0)]]
var pixels : StructuredBuffer<float4>;
[[vk::binding(2, 0)]]
[[vk::image_format("rgba8")]]
var output_image : RWTexture2D<float4>;
[[vk::binding(3, 0)]]
var exposure_state : StructuredBuffer<display::ExposureState>;
[[vk::binding(4, 0)]]
var exposure_parameters : StructuredBuffer<uint4>;

func present_pixel<let automatic : bool>(id: uint3)->void
{
    let params : Params = parameters[0];
    if (any(id.xy >= params.image.xy))
        return;
    let hdr : float4 = pixels[id.y * params.image.x + id.x];
    var drt : display::PrimeDrtParameters = params.drt;
    if (automatic)
        drt.exposure_multiplier *= exp2(exposure_state[0].exposure_ev);
    let target = uint2(id.x, params.image.y - 1 - id.y);
    output_image[target] =
        hdr.w < 0 ? float4(1, 0, 1, 1) : float4(display::prime_drt_bt709(hdr.xyz, drt), 1);
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
