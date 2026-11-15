/// 资源白名单的解码组合。每个部件只读取自身字段；未知命中的参考解码留在 mod_materials。
module mod_decode;
import mod_materials;
import pt.scene;

public interface IDecodeColor
{
    static float3 read(pt::Hit hit, uint start);
    static float3 layer(pt::Hit hit, int3 address, bool tinted);
    static float3 tint(pt::Hit hit, float3 color);
};
public struct HitColor : IDecodeColor
{
    public static float3 read(pt::Hit hit, uint start)
    {
        return hit.texel.xyz;
    }
    public static float3 layer(pt::Hit hit, int3 address, bool tinted)
    {
        float3 color = atlas.Load(address).rgb;
        return tinted ? tint(hit, color) : color;
    }
    public static float3 tint(pt::Hit hit, float3 color)
    {
        return color * quad_tint(hit.identity.x);
    }
};
struct NoColor : IDecodeColor
{
    static float3 read(pt::Hit hit, uint start)
    {
        return 0;
    }
    static float3 layer(pt::Hit hit, int3 address, bool tinted)
    {
        return 0;
    }
    static float3 tint(pt::Hit hit, float3 color)
    {
        return 0;
    }
};
public struct AtlasColor : IDecodeColor
{
    public static float3 layer(pt::Hit hit, int3 address, bool tinted)
    {
        return HitColor.layer(hit, address, tinted);
    }
    public static float3 tint(pt::Hit hit, float3 color)
    {
        return HitColor.tint(hit, color);
    }
    public static float3 read(pt::Hit hit, uint start)
    {
        float2 bary = asfloat(hit.identity.yz);
        return atlas.Load(sprite_address(material_uv(hit.identity.x, bary), material_bounds(start)))
                   .rgb *
               quad_tint(hit.identity.x);
    }
};
public interface IDecodeAux
{
    float4 read(pt::Hit hit, uint start);
};
public struct CachedAux : IDecodeAux
{
    public float4 value;
    public float4 read(pt::Hit hit, uint start)
    {
        return value;
    }
};
public struct FetchAux : IDecodeAux
{
    public float4 read(pt::Hit hit, uint start)
    {
        return aux_atlas.Load(sprite_address(material_uv(hit.identity.x, asfloat(hit.identity.yz)),
                                             material_bounds(start)));
    }
};

public float4 material_bounds(uint start)
{
    return asfloat(material_words.Load4(start + 80u));
}

interface IWeights
{
    static float3 read(uint start, int3 address);
};
struct UniformWeights : IWeights
{
    static float3 read(uint start, int3 address)
    {
        return float3(asfloat(material_words.Load(start + 12u)),
                      asfloat(material_words.Load2(start + 20u)));
    }
};
struct MappedWeights : IWeights
{
    static float3 read(uint start, int3 address)
    {
        return mers_atlas.Load(address).zxw;
    }
};

/// 层选择只携带后续真正共用的纹理地址和散射颜色，不携带整份112B材料。
struct LayerValue
{
    int3 address;
    float3 color;
    float3 weights;
};
interface IDecodeLayer
{
    static LayerValue read<C : IDecodeColor>(pt::Hit hit, uint start);
};
struct SingleLayer<W : IWeights> : IDecodeLayer
{
    static LayerValue read<C : IDecodeColor>(pt::Hit hit, uint start)
    {
        int3 address = sprite_address(material_uv(hit.identity.x, asfloat(hit.identity.yz)),
                                      material_bounds(start));
        return { address, C.read(hit, start), W.read(start, address) };
    }
};
struct UniformLayer : IDecodeLayer
{
    static LayerValue read<C : IDecodeColor>(pt::Hit hit, uint start)
    {
        return { int3(0), C.read(hit, start), UniformWeights.read(start, int3(0)) };
    }
};
struct CoveredOverlay<Top : IWeights> : IDecodeLayer
{
    static LayerValue read<C : IDecodeColor>(pt::Hit hit, uint start)
    {
        float2 uv = material_uv(hit.identity.x, asfloat(hit.identity.yz));
        float4 bounds = material_bounds(start), top = asfloat(material_words.Load4(start + 96u));
        int3 address =
            sprite_address(lerp(top.xy, top.zw, (uv - bounds.xy) / (bounds.zw - bounds.xy)), top);
        return { address, C.layer(hit, address, true), Top.read(start, address) };
    }
};
struct EmptyOverlay<Base : IWeights> : IDecodeLayer
{
    static LayerValue read<C : IDecodeColor>(pt::Hit hit, uint start)
    {
        int3 address = sprite_address(material_uv(hit.identity.x, asfloat(hit.identity.yz)),
                                      material_bounds(start));
        return { address, C.layer(hit, address, false), Base.read(start, address) };
    }
};
struct OverlayLayer<Base : IWeights, Top : IWeights> : IDecodeLayer
{
    static LayerValue read<C : IDecodeColor>(pt::Hit hit, uint start)
    {
        float2 uv = material_uv(hit.identity.x, asfloat(hit.identity.yz));
        float4 bounds = material_bounds(start);
        float4 topBounds = asfloat(material_words.Load4(start + 96u));
        float2 local = (uv - bounds.xy) / (bounds.zw - bounds.xy);
        int3 topAddress = sprite_address(lerp(topBounds.xy, topBounds.zw, local), topBounds);
        float4 top = atlas.Load(topAddress);
        if (top.a >= 0.5)
            return { topAddress, C.tint(hit, top.rgb), Top.read(start, topAddress) };
        int3 address = sprite_address(uv, bounds);
        return { address, C.layer(hit, address, false), Base.read(start, address) };
    }
};

interface IColorResponse
{
    static float3 read(uint start, float3 color);
};
struct ScatteringColor : IColorResponse
{
    static float3 read(uint start, float3 color)
    {
        return color * asfloat(material_words.Load3(start));
    }
};
struct BlackScattering : IColorResponse
{
    static float3 read(uint start, float3 color)
    {
        return 0;
    }
};
interface IEmission
{
    static float3 read(pt::Hit hit, uint start, int3 address, float scale);
};
struct NoEmission : IEmission
{
    static float3 read(pt::Hit hit, uint start, int3 address, float scale)
    {
        return 0;
    }
};
struct ConstantEmission<let Sides : uint> : IEmission
{
    static float3 read(pt::Hit hit, uint start, int3 address, float scale)
    {
        if (((hit.front ? 1u : 2u) & Sides) == 0u)
            return 0;
        float4 emission = asfloat(material_words.Load4(start + 48u));
        return emission.xyz * emission.w * hit.texel.xyz * scale;
    }
};
struct TexturedEmission<let Sides : uint> : IEmission
{
    static float3 read(pt::Hit hit, uint start, int3 address, float scale)
    {
        if (((hit.front ? 1u : 2u) & Sides) == 0u)
            return 0;
        float4 emission = asfloat(material_words.Load4(start + 48u));
        return emission.xyz * emission.w * hit.texel.xyz * mers_atlas.Load(address).y * scale;
    }
};
interface IBoundary
{
    static float4 medium(pt::Hit hit, uint start, float ior);
    static uint flags();
    static float thickness(uint start);
};
struct OpaqueBoundary : IBoundary
{
    static float4 medium(pt::Hit hit, uint start, float ior)
    {
        return hit.front ? float4(ior, asfloat(material_words.Load3(start + 36u)))
                         : float4(1, 0, 0, 0);
    }
    static uint flags()
    {
        return 0;
    }
    static float thickness(uint start)
    {
        return 0;
    }
};
struct SolidBoundary : IBoundary
{
    static float4 medium(pt::Hit hit, uint start, float ior)
    {
        return hit.front ? float4(ior, asfloat(material_words.Load3(start + 36u)))
                         : float4(1, 0, 0, 0);
    }
    static uint flags()
    {
        return 1;
    }
    static float thickness(uint start)
    {
        return 0;
    }
};
struct ThinBoundary : IBoundary
{
    static float4 medium(pt::Hit hit, uint start, float ior)
    {
        return float4(ior, asfloat(material_words.Load3(start + 36u)));
    }
    static uint flags()
    {
        return 3;
    }
    static float thickness(uint start)
    {
        return asfloat(material_words.Load(start + 28u));
    }
};
interface IAuxResponse
{
    static float4 read<A : IDecodeAux>(pt::Hit hit, uint start, A source);
    static float ior(uint start, float4 auxiliary);
    static float medium_ior(uint start, float4 auxiliary);
    static float roughness(float roughness, float4 auxiliary);
    static float3 normal(pt::Hit hit);
};
struct UniformSurface : IAuxResponse
{
    static float4 read<A : IDecodeAux>(pt::Hit hit, uint start, A source)
    {
        return 0;
    }
    static float ior(uint start, float4 auxiliary)
    {
        return asfloat(material_words.Load(start + 16u));
    }
    static float medium_ior(uint start, float4 auxiliary)
    {
        return asfloat(material_words.Load(start + 32u));
    }
    static float roughness(float roughness, float4 auxiliary)
    {
        return roughness;
    }
    static float3 normal(pt::Hit hit)
    {
        return 0;
    }
};
struct MappedIor : IAuxResponse
{
    static float4 read<A : IDecodeAux>(pt::Hit hit, uint start, A source)
    {
        return source.read(hit, start);
    }
    static float ior(uint start, float4 auxiliary)
    {
        return 1.0 + 1.5 * (round(saturate(auxiliary.w) * 255.0) * (1.0 / 255.0));
    }
    static float medium_ior(uint start, float4 auxiliary)
    {
        return ior(start, auxiliary);
    }
    static float roughness(float roughness, float4 auxiliary)
    {
        return roughness;
    }
    static float3 normal(pt::Hit hit)
    {
        return 0;
    }
};
struct MappedNormal : IAuxResponse
{
    static float4 read<A : IDecodeAux>(pt::Hit hit, uint start, A source)
    {
        return source.read(hit, start);
    }
    static float ior(uint start, float4 auxiliary)
    {
        return MappedIor.ior(start, auxiliary);
    }
    static float medium_ior(uint start, float4 auxiliary)
    {
        return ior(start, auxiliary);
    }
    static float roughness(float roughness, float4 auxiliary)
    {
        float alpha = roughness * roughness, distribution = auxiliary.z * auxiliary.z;
        return sqrt(sqrt(min(alpha * alpha + distribution * distribution, 1.0)));
    }
    static float3 normal(pt::Hit hit)
    {
        return decode_shading_normal(hit.identity.w);
    }
};

// 这里只把已校验的资源键翻译成部件类型；数值实现不再接收 Flow/Variant 掩码。
#if defined(OPENPBR_FLOW) && defined(OPENPBR_VARIANT) && OPENPBR_VARIANT != 0xffffffffu
#if OPENPBR_VARIANT & 1
typedef MappedWeights BaseWeights;
#else
typedef UniformWeights BaseWeights;
#endif
#if OPENPBR_VARIANT & 512
typedef MappedWeights TopWeights;
#else
typedef UniformWeights TopWeights;
#endif
#if OPENPBR_VARIANT & 16384
typedef CoveredOverlay<TopWeights> Layers;
#elif OPENPBR_VARIANT & 32768
typedef EmptyOverlay<BaseWeights> Layers;
#elif OPENPBR_VARIANT & 256
typedef OverlayLayer<BaseWeights, TopWeights> Layers;
#elif OPENPBR_VARIANT & 5
typedef SingleLayer<BaseWeights> Layers;
#else
typedef UniformLayer Layers;
#endif
#if (OPENPBR_FLOW & 13) && !(OPENPBR_VARIANT & 8192)
typedef ScatteringColor ColorResponse;
#else
typedef BlackScattering ColorResponse;
#endif
#if !(OPENPBR_FLOW & 1024)
typedef NoEmission EmissionResponse;
#elif OPENPBR_VARIANT & 4
static const uint EmissionSides = (OPENPBR_VARIANT >> 4) & 3;
typedef TexturedEmission<EmissionSides> EmissionResponse;
#else
static const uint EmissionSides = (OPENPBR_VARIANT >> 4) & 3;
typedef ConstantEmission<EmissionSides> EmissionResponse;
#endif
#if OPENPBR_VARIANT & 128
typedef ThinBoundary Boundary;
#elif OPENPBR_VARIANT & 64
typedef SolidBoundary Boundary;
#else
typedef OpaqueBoundary Boundary;
#endif
#if OPENPBR_VARIANT & 8
typedef MappedNormal AuxResponse;
#elif OPENPBR_VARIANT & 2
typedef MappedIor AuxResponse;
#else
typedef UniformSurface AuxResponse;
#endif

public pt::SurfaceMaterial decode_surface<C : IDecodeColor, A : IDecodeAux>(pt::Hit hit,
                                                                            float emission_scale,
                                                                            A auxiliary)
{
    uint start = material_id(hit.identity.x) * 112u;
#if (OPENPBR_FLOW & 13) && !(OPENPBR_VARIANT & 8192)
    LayerValue layer = Layers.read<C>(hit, start);
#else
    LayerValue layer = Layers.read<NoColor>(hit, start);
#endif
    float4 aux = AuxResponse.read(hit, start, auxiliary);
    float ior = AuxResponse.ior(start, aux);
    pt::SurfaceMaterial result = {};
    result.pbr.base_roughness =
        float4(ColorResponse.read(start, layer.color), AuxResponse.roughness(layer.weights.x, aux));
    result.pbr.surface = float4(ior, layer.weights.yz, Boundary.thickness(start));
    result.pbr.control.x = Boundary.flags();
    result.normal = AuxResponse.normal(hit);
    result.transmitted = Boundary.medium(hit, start, AuxResponse.medium_ior(start, aux));
    // 命中发光的颜色契约是底图；overlay只选择散射和MERS。
#if (OPENPBR_FLOW & 1024) && !(OPENPBR_VARIANT & 256)
    hit.texel.xyz = C.read(hit, start);
#endif
    result.emission = EmissionResponse.read(hit, start, layer.address, emission_scale);
    result.boundary_filter = 1;
    return result;
}
#else
// raygen也导入叶子声明，但没有材料键；只有探针/通用参考允许执行此入口。
public pt::SurfaceMaterial decode_surface<C : IDecodeColor, A : IDecodeAux>(pt::Hit hit,
                                                                            float emission_scale,
                                                                            A auxiliary)
{
    uint start = material_id(hit.identity.x) * 112u;
    hit.texel.xyz = C.read(hit, start);
    return resolve_material<true>(hit, ~0u, ~0u, emission_scale, auxiliary.read(hit, start));
}
#endif
