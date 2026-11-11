module mod_coverage;
namespace prime_coverage
{
public float2 material_uv(StructuredBuffer<uint4> quads, uint primitive, float2 barycentrics)
{
    uint base = (primitive >> 1) * 3u;
    bool second = (primitive & 1u) != 0;
    float4 uv01 = asfloat(quads[base]), uv23 = asfloat(quads[base + 1u]);
    float3 bary = float3(1.0 - barycentrics.x - barycentrics.y, barycentrics);
    float2 uv = 0;
    uv += uv01.xy * bary.x;
    uv += (second ? uv23.xy : uv01.zw) * bary.y;
    uv += (second ? uv23.zw : uv23.xy) * bary.z;
    return uv;
}
// 材质图按最近点读取。UV 在 sprite 边缘或越界时仍落在准备阶段扫描的像素矩形内。
public int3 sprite_address(Texture2D<float4> atlas, float2 uv, float4 bounds)
{
    uint w, h;
    atlas.GetDimensions(w, h);
    float2 size = float2(w, h);
    int2 lo = clamp(int2(floor(bounds.xy * size)), int2(0), int2(w - 1, h - 1));
    int2 hi = clamp(int2(ceil(bounds.zw * size)) - 1, lo, int2(w - 1, h - 1));
    return int3(clamp(int2(floor(uv * size)), lo, hi), 0);
}

public bool accepts<let Mask : uint>(StructuredBuffer<uint4> quads, Texture2D<float4> atlas,
                                     ByteAddressBuffer materials, uint primitive, float2 bary,
                                     bool front)
{
    if (Mask == 0)
        return true;
    uint start = quads[(primitive >> 1) * 3u + 2u].w * 112u;
    uint flags = materials.Load(start + 72u);
    if ((Mask & 32u) != 0 && !front && (flags & 32u) != 0)
        return false;
    if ((Mask & 8u) == 0 || (flags & 8u) == 0)
        return true;
    return atlas
               .Load(sprite_address(atlas, material_uv(quads, primitive, bary),
                                    asfloat(materials.Load4(start + 80u))))
               .a >= 0.5;
}
}
