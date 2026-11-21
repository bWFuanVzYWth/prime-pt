/// 资源白名单的解码组合。每个部件只读取自身字段；未知命中的参考解码留在 mod_materials。
module mod_decode;
import mod_materials;
import pt.scene;
import transport.surface_material;

public interface IDecodeColor
{
    static func read(hit: pt::Hit, start: uint)->float3;
    static func layer(hit: pt::Hit, address: int3, tinted: bool)->float3;
    static func tint(hit: pt::Hit, color: float3)->float3;
};
public struct HitColor : IDecodeColor
{
    public static func read(hit: pt::Hit, start: uint)->float3
    {
        return hit.texel.xyz;
    }
    public static func layer(hit: pt::Hit, address: int3, tinted: bool)->float3
    {
        let color : float3 = atlas.Load(address).rgb;
        return tinted ? tint(hit, color) : color;
    }
    public static func tint(hit: pt::Hit, color: float3)->float3
    {
        return color * quad_tint(hit.identity.x);
    }
};
struct NoColor : IDecodeColor
{
    static func read(hit: pt::Hit, start: uint)->float3
    {
        return 0;
    }
    static func layer(hit: pt::Hit, address: int3, tinted: bool)->float3
    {
        return 0;
    }
    static func tint(hit: pt::Hit, color: float3)->float3
    {
        return 0;
    }
};
public struct AtlasColor : IDecodeColor
{
    public static func layer(hit: pt::Hit, address: int3, tinted: bool)->float3
    {
        return HitColor.layer(hit, address, tinted);
    }
    public static func tint(hit: pt::Hit, color: float3)->float3
    {
        return HitColor.tint(hit, color);
    }
    public static func read(hit: pt::Hit, start: uint)->float3
    {
        let bary : float2 = asfloat(hit.identity.yz);
        return atlas.Load(sprite_address(material_uv(hit.identity.x, bary), material_bounds(start)))
                   .rgb *
               quad_tint(hit.identity.x);
    }
};
public interface IDecodeAux
{
    func read(hit: pt::Hit, start: uint)->float4;
};
public struct CachedAux : IDecodeAux
{
    public var value : float4;
    public func read(hit: pt::Hit, start: uint)->float4
    {
        return value;
    }
};
public struct FetchAux : IDecodeAux
{
    public func read(hit: pt::Hit, start: uint)->float4
    {
        return aux_atlas.Load(sprite_address(material_uv(hit.identity.x, asfloat(hit.identity.yz)),
                                             material_bounds(start)));
    }
};

public func material_bounds(start: uint)->float4
{
    return asfloat(material_words.Load4(start + 80u));
}

interface IWeights
{
    static func read(start: uint, address: int3)->float3;
};
struct UniformWeights : IWeights
{
    static func read(start: uint, address: int3)->float3
    {
        return float3(asfloat(material_words.Load(start + 12u)),
                      asfloat(material_words.Load2(start + 20u)));
    }
};
struct MappedWeights : IWeights
{
    static func read(start: uint, address: int3)->float3
    {
        return mers_atlas.Load(address).zxw;
    }
};

/// 层选择只携带后续真正共用的纹理地址和散射颜色，不携带整份112B材料。
struct LayerValue
{
    var address : int3;
    var color : float3;
    var weights : float3;
};
interface IDecodeLayer
{
    static func read<C : IDecodeColor>(hit: pt::Hit, start: uint)->LayerValue;
};
struct SingleLayer<W : IWeights> : IDecodeLayer
{
    static func read<C : IDecodeColor>(hit: pt::Hit, start: uint)->LayerValue
    {
        let address : int3 = sprite_address(material_uv(hit.identity.x, asfloat(hit.identity.yz)),
                                            material_bounds(start));
        return { address, C.read(hit, start), W.read(start, address) };
    }
};
struct UniformLayer : IDecodeLayer
{
    static func read<C : IDecodeColor>(hit: pt::Hit, start: uint)->LayerValue
    {
        return { int3(0), C.read(hit, start), UniformWeights.read(start, int3(0)) };
    }
};
struct CoveredOverlay<Top : IWeights> : IDecodeLayer
{
    static func read<C : IDecodeColor>(hit: pt::Hit, start: uint)->LayerValue
    {
        let uv : float2 = material_uv(hit.identity.x, asfloat(hit.identity.yz));
        let bounds : float4 = material_bounds(start);
        let top : float4 = asfloat(material_words.Load4(start + 96u));
        let address : int3 = sprite_address(
                          lerp(top.xy, top.zw, (uv - bounds.xy) / (bounds.zw - bounds.xy)), top);
        return { address, C.layer(hit, address, true), Top.read(start, address) };
    }
};
struct EmptyOverlay<Base : IWeights> : IDecodeLayer
{
    static func read<C : IDecodeColor>(hit: pt::Hit, start: uint)->LayerValue
    {
        let address : int3 = sprite_address(material_uv(hit.identity.x, asfloat(hit.identity.yz)),
                                            material_bounds(start));
        return { address, C.layer(hit, address, false), Base.read(start, address) };
    }
};
struct OverlayLayer<Base : IWeights, Top : IWeights> : IDecodeLayer
{
    static func read<C : IDecodeColor>(hit: pt::Hit, start: uint)->LayerValue
    {
        let uv : float2 = material_uv(hit.identity.x, asfloat(hit.identity.yz));
        let bounds : float4 = material_bounds(start);
        let top_bounds : float4 = asfloat(material_words.Load4(start + 96u));
        let local : float2 = (uv - bounds.xy) / (bounds.zw - bounds.xy);
        let top_address : int3 =
                              sprite_address(lerp(top_bounds.xy, top_bounds.zw, local), top_bounds);
        let top : float4 = atlas.Load(top_address);
        if (top.a >= 0.5)
            return { top_address, C.tint(hit, top.rgb), Top.read(start, top_address) };
        let address : int3 = sprite_address(uv, bounds);
        return { address, C.layer(hit, address, false), Base.read(start, address) };
    }
};

interface IColorResponse
{
    static func read(start: uint, color: float3)->float3;
};
struct ScatteringColor : IColorResponse
{
    static func read(start: uint, color: float3)->float3
    {
        return color * asfloat(material_words.Load3(start));
    }
};
struct BlackScattering : IColorResponse
{
    static func read(start: uint, color: float3)->float3
    {
        return 0;
    }
};
interface IEmission
{
    static func read(hit: pt::Hit, start: uint, address: int3, scale: float)->float3;
};
struct NoEmission : IEmission
{
    static func read(hit: pt::Hit, start: uint, address: int3, scale: float)->float3
    {
        return 0;
    }
};
struct ConstantEmission<let sides : uint> : IEmission
{
    static func read(hit: pt::Hit, start: uint, address: int3, scale: float)->float3
    {
        if (((hit.front ? 1u : 2u) & sides) == 0u)
            return 0;
        let emission : float4 = asfloat(material_words.Load4(start + 48u));
        return emission.xyz * emission.w * hit.texel.xyz * scale;
    }
};
struct TexturedEmission<let sides : uint> : IEmission
{
    static func read(hit: pt::Hit, start: uint, address: int3, scale: float)->float3
    {
        if (((hit.front ? 1u : 2u) & sides) == 0u)
            return 0;
        let emission : float4 = asfloat(material_words.Load4(start + 48u));
        return emission.xyz * emission.w * hit.texel.xyz * mers_atlas.Load(address).y * scale;
    }
};
interface IBoundary
{
    static func medium(hit: pt::Hit, start: uint, ior: float)->float4;
    static func flags()->uint;
    static func thickness(start: uint)->float;
};
struct OpaqueBoundary : IBoundary
{
    static func medium(hit: pt::Hit, start: uint, ior: float)->float4
    {
        return hit.front ? float4(ior, asfloat(material_words.Load3(start + 36u)))
                         : float4(1, 0, 0, 0);
    }
    static func flags()->uint
    {
        return 0;
    }
    static func thickness(start: uint)->float
    {
        return 0;
    }
};
struct SolidBoundary : IBoundary
{
    static func medium(hit: pt::Hit, start: uint, ior: float)->float4
    {
        return hit.front ? float4(ior, asfloat(material_words.Load3(start + 36u)))
                         : float4(1, 0, 0, 0);
    }
    static func flags()->uint
    {
        return 1;
    }
    static func thickness(start: uint)->float
    {
        return 0;
    }
};
struct ThinBoundary : IBoundary
{
    static func medium(hit: pt::Hit, start: uint, ior: float)->float4
    {
        return float4(ior, asfloat(material_words.Load3(start + 36u)));
    }
    static func flags()->uint
    {
        return 3;
    }
    static func thickness(start: uint)->float
    {
        return asfloat(material_words.Load(start + 28u));
    }
};
interface IAuxResponse
{
    static func read<A : IDecodeAux>(hit: pt::Hit, start: uint, source: A)->float4;
    static func ior(start: uint, auxiliary: float4)->float;
    static func medium_ior(start: uint, auxiliary: float4)->float;
    static func roughness(roughness: float, auxiliary: float4)->float;
    static func normal(hit: pt::Hit)->float3;
};
struct UniformSurface : IAuxResponse
{
    static func read<A : IDecodeAux>(hit: pt::Hit, start: uint, source: A)->float4
    {
        return 0;
    }
    static func ior(start: uint, auxiliary: float4)->float
    {
        return asfloat(material_words.Load(start + 16u));
    }
    static func medium_ior(start: uint, auxiliary: float4)->float
    {
        return asfloat(material_words.Load(start + 32u));
    }
    static func roughness(roughness: float, auxiliary: float4)->float
    {
        return roughness;
    }
    static func normal(hit: pt::Hit)->float3
    {
        return 0;
    }
};
struct MappedIor : IAuxResponse
{
    static func read<A : IDecodeAux>(hit: pt::Hit, start: uint, source: A)->float4
    {
        return source.read(hit, start);
    }
    static func ior(start: uint, auxiliary: float4)->float
    {
#if defined(OPENPBR_VARIANT) && (OPENPBR_VARIANT & 65536)
        return lab_ior(auxiliary.w);
#else
        return 1.0 + 1.5 * (round(saturate(auxiliary.w) * 255.0) * (1.0 / 255.0));
#endif
    }
    static func medium_ior(start: uint, auxiliary: float4)->float
    {
        return ior(start, auxiliary);
    }
    static func roughness(roughness: float, auxiliary: float4)->float
    {
        return roughness;
    }
    static func normal(hit: pt::Hit)->float3
    {
        return 0;
    }
};
struct MappedNormal : IAuxResponse
{
    static func read<A : IDecodeAux>(hit: pt::Hit, start: uint, source: A)->float4
    {
        return source.read(hit, start);
    }
    static func ior(start: uint, auxiliary: float4)->float
    {
        return MappedIor.ior(start, auxiliary);
    }
    static func medium_ior(start: uint, auxiliary: float4)->float
    {
        return ior(start, auxiliary);
    }
    static func roughness(roughness: float, auxiliary: float4)->float
    {
        let alpha : float = roughness * roughness;
        let distribution : float = auxiliary.z * auxiliary.z;
        return sqrt(sqrt(min(alpha * alpha + distribution * distribution, 1.0)));
    }
    static func normal(hit: pt::Hit)->float3
    {
        return decode_shading_normal(hit.identity.w);
    }
};

// 这里只把已校验的资源键翻译成部件类型；数值实现不再接收 Flow/variant 掩码。
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
static const let emission_sides : uint = (OPENPBR_VARIANT >> 4) & 3;
typedef TexturedEmission<emission_sides> EmissionResponse;
#else
static const let emission_sides : uint = (OPENPBR_VARIANT >> 4) & 3;
typedef ConstantEmission<emission_sides> EmissionResponse;
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

public func decode_surface<C : IDecodeColor, A : IDecodeAux>(input_hit: pt::Hit,
                                                             emission_scale: float, auxiliary: A)
    ->transport::SurfaceMaterial
{
    var hit = input_hit;
    let start : uint = material_id(hit.identity.x) * 112u;
#if (OPENPBR_FLOW & 13) && !(OPENPBR_VARIANT & 8192)
    var layer : LayerValue = Layers.read<C>(hit, start);
#else
    let layer : LayerValue = Layers.read<NoColor>(hit, start);
#endif
    let aux : float4 = AuxResponse.read(hit, start, auxiliary);
    let ior : float = AuxResponse.ior(start, aux);
    var result : transport::SurfaceMaterial = {};
    result.pbr.base_roughness =
        float4(ColorResponse.read(start, layer.color), AuxResponse.roughness(layer.weights.x, aux));
#if OPENPBR_VARIANT & 65536
    result.pbr.base_roughness.xyz *= lab_metal_tint(aux.w);
#endif
    result.pbr.surface = float4(ior, layer.weights.yz, Boundary.thickness(start));
#if !(OPENPBR_FLOW & 8)
    result.pbr.surface.z = 0;
#endif
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
public func decode_surface<C : IDecodeColor, A : IDecodeAux>(input_hit: pt::Hit,
                                                             emission_scale: float, auxiliary: A)
    ->transport::SurfaceMaterial
{
    var hit = input_hit;
    let start : uint = material_id(hit.identity.x) * 112u;
    hit.texel.xyz = C.read(hit, start);
    return resolve_material<true>(hit, ~0u, ~0u, emission_scale, auxiliary.read(hit, start));
}
#endif
