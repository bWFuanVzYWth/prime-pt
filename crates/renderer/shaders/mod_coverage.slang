module mod_coverage;
namespace prime_coverage
{
public func material_uv(quads: StructuredBuffer<uint4>, primitive: uint, barycentrics: float2)
    ->float2
{
    let base : uint = (primitive >> 1) * 3u;
    let second : bool = (primitive & 1u) != 0;
    let uv01 : float4 = asfloat(quads[base]);
    let uv23 : float4 = asfloat(quads[base + 1u]);
    let bary = float3(1.0 - barycentrics.x - barycentrics.y, barycentrics);
    var uv : float2 = 0;
    uv += uv01.xy * bary.x;
    uv += (second ? uv23.xy : uv01.zw) * bary.y;
    uv += (second ? uv23.zw : uv23.xy) * bary.z;
    return uv;
}
// 材质图按最近点读取。UV 在 sprite 边缘或越界时仍落在准备阶段扫描的像素矩形内。
public func sprite_address(atlas: Texture2D<float4>, uv: float2, bounds: float4)->int3
{
    var w : uint;
    var h : uint;
    atlas.GetDimensions(w, h);
    let size = float2(w, h);
    let lo : int2 = clamp(int2(floor(bounds.xy * size)), int2(0), int2(w - 1, h - 1));
    let hi : int2 = clamp(int2(ceil(bounds.zw * size)) - 1, lo, int2(w - 1, h - 1));
    return int3(clamp(int2(floor(uv * size)), lo, hi), 0);
}

public func accepts<let mask : uint>(quads: StructuredBuffer<uint4>, atlas: Texture2D<float4>,
                                     materials: ByteAddressBuffer, primitive: uint, bary: float2,
                                     front: bool)
    ->bool
{
    if (mask == 0)
        return true;
    let start : uint = quads[(primitive >> 1) * 3u + 2u].w * 112u;
    let flags : uint = materials.Load(start + 72u);
    if ((mask & 32u) != 0 && !front && (flags & 32u) != 0)
        return false;
    if ((mask & 8u) == 0 || (flags & 8u) == 0)
        return true;
    return atlas
               .Load(sprite_address(atlas, material_uv(quads, primitive, bary),
                                    asfloat(materials.Load4(start + 80u))))
               .a >= 0.5;
}
}
