/// 左上原点线性 HDR 到宿主 RGBA8_UNORM；第 0 行对应宿主画面底部。
module present;
import params;
import display.prime_drt;

[[vk::binding(0, 0)]]
StructuredBuffer<Params> parameters;
[[vk::binding(1, 0)]]
StructuredBuffer<float4> pixels;
[[vk::binding(2, 0)]]
[[vk::image_format("rgba8")]]
RWTexture2D<float4> output_image;

[shader("compute")]
[numthreads(8, 8, 1)]
void present_main(uint3 id: SV_DispatchThreadID)
{
    Params params = parameters[0];
    if (any(id.xy >= params.image.xy))
        return;
    float4 hdr = pixels[id.y * params.image.x + id.x];
    uint2 target = uint2(id.x, params.image.y - 1 - id.y);
    output_image[target] =
        hdr.w < 0 ? float4(1, 0, 1, 1) : float4(display::prime_drt_bt709(hdr.xyz, params.drt), 1);
}
