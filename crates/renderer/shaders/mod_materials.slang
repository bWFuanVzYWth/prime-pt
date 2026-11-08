/// 资源世代的材质表、真实quad UV/tint与PBR纹理；不查询宿主或拥有GPU资源。
module mod_materials;
import pt.scene;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;

#ifndef PRIMEPT_COVERAGE_MASK
#define PRIMEPT_COVERAGE_MASK 40u
#endif
public static const uint COVERAGE_MASK = PRIMEPT_COVERAGE_MASK;
#ifndef PRIMEPT_EMITTER_CAPABILITIES
#define PRIMEPT_EMITTER_CAPABILITIES 7u
#endif
// 资源期已发布灯域：alias、cutout、MERS纹理发光；参考入口保守保留全部能力。
public static const uint EMITTER_CAPABILITIES = PRIMEPT_EMITTER_CAPABILITIES;
static const uint MATERIAL_OVERLAY_FLAGS = (1u << 7u) | (1u << 8u);
public static const uint VARIANT_OVERLAY = 1u << 8u;
static const uint VARIANT_OVERLAY_MERS = 1u << 9u;
static const uint VARIANT_BLACK_CONDUCTOR = 1u << 13u;

public bool reads_scattering_color(uint flow, uint variant)
{
    return (variant == ~0u || (variant & VARIANT_BLACK_CONDUCTOR) == 0) &&
           (flow & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                    openpbr::FLOW_CONDUCTOR_REFLECTION)) != 0;
}

[[vk::binding(1, 0)]]
public StructuredBuffer<uint4> quads;
[[vk::binding(2, 0)]]
public Texture2D<float4> atlas;
[[vk::binding(3, 0)]]
public SamplerState atlas_sampler;

/// GPU派生的112B七块表，物理字段与prime_scene::MaterialDefinition保持同位。
/// control.x低2位为源OpenPBR flags，高30位由资源准备写入callable槽，不回写CPU定义。
/// control.z高12位为GPU派生FlowKey，低20位保留原flags；身份与材质参数均不截断。
/// control.y低2位为发光侧，高30位指向同buffer尾部的资源期操作槽记录（word地址）。
public struct ModMaterial
{
    public float4 base_roughness;
    public float4 surface;
    public float4 extinction;
    public float4 emission;
    public uint4 control;
    public float4 uv_bounds;
    public float4 overlay_bounds;
};
[[vk::binding(27, 0)]]
public ByteAddressBuffer material_words;
[[vk::binding(28, 0)]]
public Texture2D<float4> mers_atlas;
[[vk::binding(29, 0)]]
public Texture2D<float4> aux_atlas;

/// 前112B/材质保持源布局；逐块load供最终消费者裁剪，尾部操作表不进入材料值。
public ModMaterial material_at(uint index)
{
    uint start = index * 112u;
    return { asfloat(material_words.Load4(start)),       asfloat(material_words.Load4(start + 16u)),
             asfloat(material_words.Load4(start + 32u)), asfloat(material_words.Load4(start + 48u)),
             material_words.Load4(start + 64u),          asfloat(material_words.Load4(start + 80u)),
             asfloat(material_words.Load4(start + 96u)) };
}

public static const uint MATERIAL_INITIAL = 0u;
public static const uint MATERIAL_REPLAY = 1u;
public static const uint MATERIAL_EVALUATE = 2u;
public static const uint MATERIAL_METADATA = 3u;
public static const uint MATERIAL_CONNECTION = 4u;
public static const uint MATERIAL_EMITTER = 5u;
public static const uint MATERIAL_PDF = 6u;
public static const uint MATERIAL_GUIDE = 7u;
public static const uint MATERIAL_INITIAL_DIRECT = 8u;

public uint material_program(uint primitive, uint operation)
{
    uint pointer = material_words.Load(material_id(primitive) * 112u + 68u) >> 2u;
    return material_words.Load((pointer + operation) * 4u);
}

/// metadata只在known-surface shift表中调度；初采由完整专用leaf一次求值。
public uint metadata_program(uint primitive)
{
    uint2 control = material_words.Load2(material_id(primitive) * 112u + 64u);
    uint pointer = control.y >> 2u;
    uint local = material_words.Load((pointer + MATERIAL_METADATA) * 4u);
    uint header = pointer - (control.x >> 2u) * 9u - 8u;
    return local + material_words.Load((header + 1u) * 4u);
}

/// 独立Guide SBT只有本操作的紧凑槽，不读取其他积分器的段偏移。
public uint guide_slot(uint primitive)
{
    uint pointer = material_words.Load(material_id(primitive) * 112u + 68u) >> 2u;
    return material_words.Load((pointer + MATERIAL_GUIDE) * 4u);
}

public uint guide_program(uint primitive, bool known, bool restir)
{
    uint2 control = material_words.Load2(material_id(primitive) * 112u + 64u);
    uint pointer = control.y >> 2u;
    uint local = material_words.Load((pointer + MATERIAL_GUIDE) * 4u);
    uint header = pointer - (control.x >> 2u) * 9u - 8u;
#ifdef PRIMEPT_INITIAL_HIT
    uint index = known ? 4u : (restir ? 5u : 2u);
#else
    uint index = known ? 3u : (restir ? 5u : 2u);
#endif
    return local + material_words.Load((header + index) * 4u);
}

public uint material_id(uint primitive)
{
    return quads[(primitive >> 1) * 3u + 2u].w;
}
public float3 material_linear(float3 value)
{
    return select(value <= 0.04045, value / 12.92, pow((value + 0.055) / 1.055, 2.4));
}
public float2 material_uv(uint primitive, float2 barycentrics)
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
public int3 sprite_address(float2 uv, float4 bounds)
{
    uint w, h;
    atlas.GetDimensions(w, h);
    float2 size = float2(w, h);
    int2 lo = clamp(int2(floor(bounds.xy * size)), int2(0), int2(w - 1, h - 1));
    int2 hi = clamp(int2(ceil(bounds.zw * size)) - 1, lo, int2(w - 1, h - 1));
    return int3(clamp(int2(floor(uv * size)), lo, hi), 0);
}

public float4 material_aux<let Variant : uint = ~0u>(uint primitive, float2 bary)
{
    if ((Variant & 2u) == 0)
        return 0;
    float4 bounds = asfloat(material_words.Load4(material_id(primitive) * 112u + 80u));
    return aux_atlas.Load(sprite_address(material_uv(primitive, bary), bounds));
}

public float3 quad_tint(uint primitive, float2 barycentrics)
{
    float3 bary = float3(1.0 - barycentrics.x - barycentrics.y, barycentrics);
    float3 shared_tint = asfloat(quads[(primitive >> 1) * 3u + 2u].xyz);
    float3 tint = 0;
    tint += shared_tint * bary.x;
    tint += shared_tint * bary.y;
    tint += shared_tint * bary.z;
    return material_linear(saturate(tint));
}
float4 sample_tinted_texel(uint primitive, float2 barycentrics, float2 uv)
{
    float4 value = atlas.Load(
        sprite_address(uv, asfloat(material_words.Load4(material_id(primitive) * 112u + 80u))));
    return float4(value.xyz * quad_tint(primitive, barycentrics), value.w);
}
/// 保留旧共享tint的FP32插值顺序；覆盖alpha不乘tint。
public float4 material_texel(uint primitive, float2 barycentrics)
{
    return sample_tinted_texel(primitive, barycentrics, material_uv(primitive, barycentrics));
}
/// CUTOUT来自实际源layer；opaque/透射界面不将alpha误解释为二值孔洞。
interface ICoverage
{
    static bool accepts(uint primitive, float2 bary, bool front);
};
struct AllFaces : ICoverage
{
    static bool accepts(uint primitive, float2 bary, bool front)
    {
        return true;
    }
};
struct FrontFaces : ICoverage
{
    static bool accepts(uint primitive, float2 bary, bool front)
    {
        return front || (material_words.Load(material_id(primitive) * 112u + 72u) & 32u) == 0;
    }
};
struct AlphaFaces<F : ICoverage> : ICoverage
{
    static bool accepts(uint primitive, float2 bary, bool front)
    {
        if (!F.accepts(primitive, bary, front))
            return false;
        uint start = material_id(primitive) * 112u;
        if ((material_words.Load(start + 72u) & 8u) == 0)
            return true;
        return atlas
                   .Load(sprite_address(material_uv(primitive, bary),
                                        asfloat(material_words.Load4(start + 80u))))
                   .a >= 0.5;
    }
};
#if PRIMEPT_COVERAGE_MASK == 0
typedef AllFaces PublishedCoverage;
#elif PRIMEPT_COVERAGE_MASK == 8
typedef AlphaFaces<AllFaces> PublishedCoverage;
#elif PRIMEPT_COVERAGE_MASK == 32
typedef FrontFaces PublishedCoverage;
#elif PRIMEPT_COVERAGE_MASK == 40
typedef AlphaFaces<FrontFaces> PublishedCoverage;
#else
#error invalid coverage recipe
#endif
public bool material_accepts(uint primitive, float2 bary, bool front)
{
    return PublishedCoverage.accepts(primitive, bary, front);
}

interface IEmissionWeight
{
    static float read(uint start, int3 address);
};
struct UniformEmission : IEmissionWeight
{
    static float read(uint start, int3 address)
    {
        return 1;
    }
};
struct MappedEmission : IEmissionWeight
{
    static float read(uint start, int3 address)
    {
        return (material_words.Load(start + 72u) & 4u) != 0 ? mers_atlas.Load(address).y : 1;
    }
};
interface IEmitterCoverage
{
    static bool accepts(uint start, float alpha);
};
struct OpaqueEmitter : IEmitterCoverage
{
    static bool accepts(uint start, float alpha)
    {
        return true;
    }
};
struct CutoutEmitter : IEmitterCoverage
{
    static bool accepts(uint start, float alpha)
    {
        return (material_words.Load(start + 72u) & 8u) == 0 || alpha >= 0.5;
    }
};
#if PRIMEPT_EMITTER_CAPABILITIES & 4
typedef MappedEmission EmissionWeight;
#else
typedef UniformEmission EmissionWeight;
#endif
#if PRIMEPT_EMITTER_CAPABILITIES & 2
typedef CutoutEmitter EmitterCoverage;
#else
typedef OpaqueEmitter EmitterCoverage;
#endif
/// 已经通过灯面side/PDF检查；只读发光消费者所需的四行字段与纹素。
public float3 sampled_material_emission(uint primitive, float2 bary, float scale)
{
    uint start = material_id(primitive) * 112u;
    int3 address =
        sprite_address(material_uv(primitive, bary), asfloat(material_words.Load4(start + 80u)));
    float4 texel = atlas.Load(address);
    if (!EmitterCoverage.accepts(start, texel.a))
        return 0;
    float4 emission = asfloat(material_words.Load4(start + 48u));
    return emission.xyz * emission.w * (texel.rgb * quad_tint(primitive, bary)) *
           EmissionWeight.read(start, address) * scale;
}
bool emits_on_side(ModMaterial material, bool front)
{
    uint side = front ? 1u : 2u;
    return (material.control.y & side) != 0 && material.emission.w > 0 &&
           ((material.control.z & 32u) == 0 || front);
}
/// AcceptedSample只用于已通过light_regions.sideMask与正PDF检查的forward样本。
/// reference/未知命中保留完整源规则，不套用仅覆盖已发布候选灯的上界。
public float3 material_emission<let AcceptedSample : bool = false>(uint primitive, float2 bary,
                                                                   bool front,
                                                                   float block_emission_scale)
{
    if (AcceptedSample)
        return sampled_material_emission(primitive, bary, block_emission_scale);
    ModMaterial material = material_at(material_id(primitive));
    if (!emits_on_side(material, front))
        return 0;
    float2 uv = material_uv(primitive, bary);
    float4 texel = sample_tinted_texel(primitive, bary, uv);
    if ((material.control.z & 8u) != 0 && texel.a < 0.5)
        return 0;
    float3 result = material.emission.xyz * material.emission.w * texel.xyz;
    if ((material.control.z & 4u) != 0)
        result *= mers_atlas.Load(sprite_address(uv, material.uv_bounds)).y;
    return result * block_emission_scale;
}
float2 normal_sign(float2 value)
{
    return float2(value.x >= 0 ? 1.0 : -1.0, value.y >= 0 ? 1.0 : -1.0);
}
public uint encode_shading_normal(float3 normal)
{
    normal /= abs(normal.x) + abs(normal.y) + abs(normal.z);
    float2 oct = normal.z >= 0 ? normal.xy : (1.0 - abs(normal.yx)) * normal_sign(normal.xy);
    uint2 value = uint2(round(saturate(oct * 0.5 + 0.5) * 65535.0));
    return value.x | (value.y << 16u);
}
public float3 decode_shading_normal(uint packed)
{
    float2 oct = float2(packed & 65535u, packed >> 16u) * (2.0 / 65535.0) - 1.0;
    float3 normal = float3(oct, 1.0 - abs(oct.x) - abs(oct.y));
    if (normal.z < 0)
        normal.xy = (1.0 - abs(normal.yx)) * normal_sign(normal.xy);
    return normalize(normal);
}
/// 只有显式法线贴图需要变换三角形边；几何法线和offset原样保留。
public uint map_normal_xy(uint primitive, float2 encoded, float3 local_e1, float3 local_e2,
                          float3x3 object_to_world, float3 normal)
{
    uint base = (primitive >> 1) * 3u;
    float4 uv01 = asfloat(quads[base]), uv23 = asfloat(quads[base + 1u]);
    bool second = (primitive & 1u) != 0;
    float2 du = (second ? uv23.xy : uv01.zw) - uv01.xy;
    float2 dv = (second ? uv23.zw : uv23.xy) - uv01.xy;
    float determinant = du.x * dv.y - du.y * dv.x;
    if (determinant == 0 || !isfinite(determinant))
        return encode_shading_normal(normal);
    float3 e1 = mul(object_to_world, local_e1);
    float3 e2 = mul(object_to_world, local_e2);
    float3 tangent = (e1 * dv.y - e2 * du.y) / determinant;
    float3 bitangent = (e2 * du.x - e1 * dv.x) / determinant;
    tangent -= normal * dot(normal, tangent);
    float length_squared = dot(tangent, tangent);
    if (!(length_squared > 0) || !isfinite(length_squared))
        return encode_shading_normal(normal);
    tangent *= rsqrt(length_squared);
    bitangent = cross(normal, tangent) * (dot(cross(normal, tangent), bitangent) < 0 ? -1.0 : 1.0);
    float2 xy = (encoded * 255.0 - 128.0) / 127.0;
    float3 local = normalize(float3(xy, sqrt(max(0.0, 1.0 - dot(xy, xy)))));
    return encode_shading_normal(
        normalize(tangent * local.x + bitangent * local.y + normal * local.z));
}
public uint mapped_shading_normal_known(uint primitive, float2 bary, float3 local_e1,
                                        float3 local_e2, float3x3 object_to_world, float3 normal)
{
    return map_normal_xy(primitive, material_aux(primitive, bary).xy, local_e1, local_e2,
                         object_to_world, normal);
}
/// 未知命中的独立导引路径；照明叶子按资源期能力直接调用known版本。
public uint mapped_shading_normal(uint primitive, float2 bary, float3 local_e1, float3 local_e2,
                                  float3x3 object_to_world, float3 normal)
{
    if ((material_words.Load(material_id(primitive) * 112u + 76u) & 1u) == 0)
        return 0;
    return mapped_shading_normal_known(primitive, bary, local_e1, local_e2, object_to_world,
                                       normal);
}
// 能力位与 materials::variant 同步。通用 guide 根据当前命中读取相同信息。
uint material_variant(ModMaterial material)
{
    uint flags = material.control.z;
    uint sides = material.control.y & ((flags & 32u) != 0 ? 1u : 3u);
    uint emission = material.emission.w > 0 ? (sides << 4u) | (flags & 4u) : 0;
    return (flags & 3u) | ((material.control.w & 1u) << 3u) | emission |
           ((material.control.x & 3u) << 6u) | ((flags & MATERIAL_OVERLAY_FLAGS) << 1u);
}

/// 消费已通过覆盖测试的命中；callable 的 Flow 和 variant 都在资源准备时特化。
public pt::SurfaceMaterial resolve_material<let HasAux : bool = false,
                                            let ReadColor : bool = false>(
    pt::Hit hit, uint flow, uint variant, float block_emission_scale, float4 sampledAux = float4(0))
{
    uint primitive = hit.identity.x;
    ModMaterial material = material_at(material_id(primitive));
    uint mask = variant == ~0u ? material_variant(material) : variant;
    pt::SurfaceMaterial result = {};
    result.pbr.base_roughness.w = material.base_roughness.w;
    result.pbr.surface = material.surface;
    result.pbr.control.x = (mask >> 6u) & 3u;
    bool emits = (flow & openpbr::FLOW_EMISSION) != 0 &&
                 ((mask & 48u) == 48u || ((mask & 16u) != 0 && hit.front) ||
                  ((mask & 32u) != 0 && !hit.front));
    bool textured_emission = emits && (mask & 4u) != 0;
    float2 uv = 0;
    bool useColor = reads_scattering_color(flow, mask);
    bool overlay = (mask & VARIANT_OVERLAY) != 0 &&
                   (useColor || (mask & (1u | VARIANT_OVERLAY_MERS)) != 0 || textured_emission);
    bool readBase = ReadColor && (mask & VARIANT_OVERLAY) == 0 && (useColor || emits);
    if (readBase || overlay || (mask & 3u) != 0 || textured_emission)
        uv = material_uv(primitive, asfloat(hit.identity.yz));
    float4 bounds = material.uv_bounds;
    if (readBase)
        hit.texel.xyz = atlas.Load(sprite_address(uv, bounds)).rgb *
                        quad_tint(primitive, asfloat(hit.identity.yz));
    float3 base_color = hit.texel.xyz;
    bool reads_mers = (mask & 1u) != 0;
    if (overlay)
    {
        float2 local =
            (uv - material.uv_bounds.xy) / (material.uv_bounds.zw - material.uv_bounds.xy);
        float2 top_uv = lerp(material.overlay_bounds.xy, material.overlay_bounds.zw, local);
        float4 top = atlas.Load(sprite_address(top_uv, material.overlay_bounds));
        if (top.a >= 0.5)
        {
            base_color = top.rgb * quad_tint(primitive, asfloat(hit.identity.yz));
            reads_mers = (mask & VARIANT_OVERLAY_MERS) != 0;
            uv = top_uv;
            bounds = material.overlay_bounds;
        }
        else
        {
            base_color = atlas.Load(sprite_address(uv, bounds)).rgb;
        }
    }
    if (useColor)
        result.pbr.base_roughness.xyz = base_color * material.base_roughness.xyz;
    float4 mers = float4(material.surface.y, 0, material.base_roughness.w, material.surface.z);
    if (reads_mers || textured_emission)
        mers = mers_atlas.Load(sprite_address(uv, bounds));
    if (reads_mers)
    {
        result.pbr.base_roughness.w = mers.z;
        result.pbr.surface.y = mers.x;
        result.pbr.surface.z = mers.w;
    }
    float4 medium = material.extinction;
    if ((mask & 2u) != 0)
    {
        float4 auxiliary = HasAux ? sampledAux : aux_atlas.Load(sprite_address(uv, bounds));
        float ior = 1.0 + 1.5 * (round(saturate(auxiliary.w) * 255.0) / 255.0);
        result.pbr.surface.x = ior;
        medium.x = ior;
        if ((mask & 8u) != 0)
        {
            float alpha = result.pbr.base_roughness.w * result.pbr.base_roughness.w;
            float distribution = auxiliary.z * auxiliary.z;
            result.pbr.base_roughness.w =
                sqrt(sqrt(min(alpha * alpha + distribution * distribution, 1.0)));
            result.normal = decode_shading_normal(hit.identity.w);
        }
    }
    bool thin = (mask & 128u) != 0;
    result.transmitted = thin || hit.front ? medium : float4(1, 0, 0, 0);
    if (emits)
    {
        result.emission = material.emission.xyz * material.emission.w * hit.texel.xyz;
        if (textured_emission)
            result.emission *= mers.y;
        result.emission *= block_emission_scale;
    }
    result.boundary_filter = 1;
    return result;
}
