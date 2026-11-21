/// 将宿主正在使用的纹理视图复制到射线材质共用的 RGBA8 表。RenderPearl 以 GENERAL 交接。
[[vk::binding(0, 0)]]
var source : Texture2D<float4>;
[[vk::binding(1, 0)]]
var pixels : RWStructuredBuffer<uint>;
[[vk::binding(2, 0)]]
var region : StructuredBuffer<uint4>;
[shader("compute")]
[numthreads(8, 8, 1)]
func feature_texture(id: uint3: SV_DispatchThreadID)->void
{
    let r = region[0];
    if (any(id.xy >= r.yz))
        return;
    var rgba = source.Load(int3(id.xy, 0));
    if (r.w != 0)
        rgba.rgb = select(rgba.rgb <= 0.0031308, rgba.rgb * 12.92,
                          1.055 * pow(rgba.rgb, 1.0 / 2.4) - 0.055);
    let v = uint4(round(saturate(rgba) * 255.0));
    pixels[r.x + id.y * r.y + id.x] = v.x | (v.y << 8u) | (v.z << 16u) | (v.w << 24u);
}
