/// 宿主实例的三角形、原版纹理和可重投影的前帧顶点。位置均相对本帧的地形原点。
module world_features;
namespace world_features
{
#ifndef PRIMEPT_WORLD_FEATURES
#define PRIMEPT_WORLD_FEATURES 1
#endif
public static const let configured = PRIMEPT_WORLD_FEATURES != 0;
public static const let instance_bit = 0x800000u;
public static const let primitive_bit = 0x80000000u;
public static const let shadow_mask = 1u;
public struct Surface
{
    public var material : uint4;     // 材质、宿主覆盖标志、纹理起点、宽度
    public var identity : uint4;     // 持久面身份、高位、纹理高度、前态有效
    public var position : float4[4]; // w 为原始 RGBA8
    public var previous : float4[4];
    public var uv : float4[2];
};
[[vk::binding(47, 0)]]
public var faces : StructuredBuffer<Surface>;
[[vk::binding(48, 0)]]
public var pixels : StructuredBuffer<uint>;
public func active(primitive: uint)->bool
{
    return configured && (primitive & primitive_bit) != 0;
}
public func slot(primitive: uint)->uint
{
    return (primitive & ~primitive_bit) >> 1;
}
public func indices(primitive: uint)->uint3
{
    return (primitive & 1u) == 0 ? uint3(0, 1, 2) : uint3(0, 2, 3);
}
func unpack_rgba(word: uint)->float4
{
    return float4(word & 255u, (word >> 8u) & 255u, (word >> 16u) & 255u, word >> 24u) / 255.0;
}
func linear_rgb(s: float3)->float3
{
    return select(s <= 0.04045, s / 12.92, pow((s + 0.055) / 1.055, 2.4));
}
func corner_uv(StructuredBuffer<Surface> source, face: uint, index: uint)->float2
{
    let pair = source[face].uv[index >> 1u];
    return (index & 1u) == 0 ? pair.xy : pair.zw;
}
public func color_at(StructuredBuffer<Surface> source, StructuredBuffer<uint> bank, primitive: uint,
                     bary: float2, terrain: Texture2D<float4>)
    ->float4
{
    let n = slot(primitive);
    let tri = indices(primitive);
    let w = float3(1 - bary.x - bary.y, bary);
    let m = source[n].material;
    let uv = corner_uv(source, n, tri.x) * w.x + corner_uv(source, n, tri.y) * w.y +
             corner_uv(source, n, tri.z) * w.z;
    let tint = unpack_rgba(asuint(source[n].position[tri.x].w)) * w.x +
               unpack_rgba(asuint(source[n].position[tri.y].w)) * w.y +
               unpack_rgba(asuint(source[n].position[tri.z].w)) * w.z;
    var color = float4(1);
    if (m.z == 0xfffffffeu)
    {
        uint width, height;
        terrain.GetDimensions(width, height);
        color = terrain.Load(int3(frac(uv) * float2(width, height), 0));
    }
    else if (m.w != 0)
    {
        let size = uint2(m.w, source[n].identity.z);
        let xy = min(uint2(frac(uv) * float2(size)), size - 1u);
        color = unpack_rgba(bank[m.z + xy.y * size.x + xy.x]);
        if ((m.y & 16u) != 0)
            color = float4(1, 1, 1, color.r);
        else
            color.rgb = linear_rgb(color.rgb);
    }
    color *= float4(linear_rgb(tint.rgb), tint.a);
    if ((m.y & 32u) != 0)
        color.rgb *= color.a;
    return color;
}
public func sample(primitive: uint, bary: float2, terrain: Texture2D<float4>)->float4
{
    return color_at(faces, pixels, primitive, bary, terrain);
}
public func accepts(StructuredBuffer<Surface> source, StructuredBuffer<uint> bank, uint primitive,
                    float2 bary, Texture2D<float4> terrain)
    ->bool
{
    let flags = source[slot(primitive)].material.y;
    if ((flags & 3u) == 0u)
        return true;
    let alpha = color_at(source, bank, primitive, bary, terrain).a;
    if ((flags & 2u) == 0)
        return alpha >= 0.5;
    if ((flags & 32u) != 0)
        return alpha > 0;
    // 将覆盖率表达为固定在原语上的亚像素面积。时空采样保留烟雾与半透明 sprite 的透光率。
    let q = uint2(saturate(bary) * 65535u);
    var h = (q.x | (q.y << 16u)) ^ source[slot(primitive)].identity.x;
    h ^= h >> 16;
    h *= 0x7feb352du;
    h ^= h >> 15;
    h *= 0x846ca68bu;
    h ^= h >> 16;
    return alpha > float(h >> 8u) * (1.0 / 16777216.0);
}
public func previous_position(primitive: uint, bary: float2)->float3
{
    let n = slot(primitive);
    let tri = indices(primitive);
    let a = faces[n].previous[tri.x].xyz;
    return a + (faces[n].previous[tri.y].xyz - a) * bary.x +
           (faces[n].previous[tri.z].xyz - a) * bary.y;
}
}
