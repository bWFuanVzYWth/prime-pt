/// 资源世代的材质表、真实quad UV/tint与PBR纹理；不查询宿主或拥有GPU资源。
module mod_materials;
import pt.scene;
import mod_coverage;
import openpbr.api;
import openpbr.flow;
import transport.surface_material;

#ifndef PRIMEPT_COVERAGE_MASK
#define PRIMEPT_COVERAGE_MASK 40u
#endif
public static const let coverage_mask : uint = PRIMEPT_COVERAGE_MASK;
#ifndef PRIMEPT_EMITTER_CAPABILITIES
#define PRIMEPT_EMITTER_CAPABILITIES 7u
#endif
// 资源期已发布灯域：alias、cutout、MERS纹理发光；参考入口保守保留全部能力。
public static const let emitter_capabilities : uint = PRIMEPT_EMITTER_CAPABILITIES;
static const let material_overlay_flags : uint = (1u << 7u) | (1u << 8u);
public static const let variant_overlay : uint = 1u << 8u;
static const let variant_overlay_mers : uint = 1u << 9u;
static const let variant_black_conductor : uint = 1u << 13u;

public func reads_scattering_color(flow: uint, variant: uint)->bool
{
    return (variant == ~0u || (variant & variant_black_conductor) == 0) &&
           (flow & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                    openpbr::FLOW_CONDUCTOR_REFLECTION)) != 0;
}

[[vk::binding(1, 0)]]
// 每quad三行：UV32B与共享FP32线性tint/material16B；CPU源quad仍为sRGB。
public var quads : StructuredBuffer<uint4>;
[[vk::binding(2, 0)]]
public var atlas : Texture2D<float4>;
[[vk::binding(3, 0)]]
public var atlas_sampler : SamplerState;

/// GPU派生的112B七块表，物理字段与prime_scene::MaterialDefinition保持同位。
/// control.x低2位为源OpenPBR flags，高30位由资源准备写入callable槽，不回写CPU定义。
/// control.z高12位为GPU派生FlowKey，低20位保留原flags；身份与材质参数均不截断。
/// control.y低2位为发光侧，高30位为资源期准备的导引槽。
public struct ModMaterial
{
    public var base_roughness : float4;
    public var surface : float4;
    public var extinction : float4;
    public var emission : float4;
    public var control : uint4;
    public var uv_bounds : float4;
    public var overlay_bounds : float4;
};
[[vk::binding(27, 0)]]
public var material_words : ByteAddressBuffer;
[[vk::binding(28, 0)]]
public var mers_atlas : Texture2D<float4>;
[[vk::binding(29, 0)]]
public var aux_atlas : Texture2D<float4>;

/// 前112B/材质保持源布局；逐块load供最终消费者裁剪，导引槽不进入物理材料值。
public func material_at(index: uint)->ModMaterial
{
    let start : uint = index * 112u;
    return { asfloat(material_words.Load4(start)),       asfloat(material_words.Load4(start + 16u)),
             asfloat(material_words.Load4(start + 32u)), asfloat(material_words.Load4(start + 48u)),
             material_words.Load4(start + 64u),          asfloat(material_words.Load4(start + 80u)),
             asfloat(material_words.Load4(start + 96u)) };
}

/// control.y 高 30 位是确定性导引的专用 SBT 槽。
public func guide_slot(primitive: uint)->uint
{
    return material_words.Load(material_id(primitive) * 112u + 68u) >> 2u;
}

public func material_id(primitive: uint)->uint
{
    return quads[(primitive >> 1) * 3u + 2u].w;
}
public func material_uv(primitive: uint, barycentrics: float2)->float2
{
    return prime_coverage::material_uv(quads, primitive, barycentrics);
}
public func sprite_address(uv: float2, bounds: float4)->int3
{
    return prime_coverage::sprite_address(atlas, uv, bounds);
}

public func material_aux<let variant : uint = ~0u>(primitive: uint, bary: float2)->float4
{
    if ((variant & 2u) == 0)
        return 0;
    let bounds : float4 = asfloat(material_words.Load4(material_id(primitive) * 112u + 80u));
    return aux_atlas.Load(sprite_address(material_uv(primitive, bary), bounds));
}

public func quad_tint(primitive: uint)->float3
{
    return asfloat(quads[(primitive >> 1) * 3u + 2u].xyz);
}
func sample_tinted_texel(primitive: uint, barycentrics: float2, uv: float2)->float4
{
    let value : float4 = atlas.Load(sprite_address(
                    uv, asfloat(material_words.Load4(material_id(primitive) * 112u + 80u))));
    return float4(value.xyz * quad_tint(primitive), value.w);
}
/// 共享tint已在GPU属性发布时解码；覆盖alpha不乘tint。
public func material_texel(primitive: uint, barycentrics: float2)->float4
{
    return sample_tinted_texel(primitive, barycentrics, material_uv(primitive, barycentrics));
}
/// CUTOUT来自实际源layer；opaque/透射界面不将alpha误解释为二值孔洞。
public func material_accepts(primitive: uint, bary: float2, front: bool)->bool
{
    return prime_coverage::accepts<PRIMEPT_COVERAGE_MASK>(quads, atlas, material_words, primitive,
                                                          bary, front);
}

interface IEmissionWeight
{
    static func read(start: uint, address: int3)->float;
};
struct UniformEmission : IEmissionWeight
{
    static func read(start: uint, address: int3)->float
    {
        return 1;
    }
};
struct MappedEmission : IEmissionWeight
{
    static func read(start: uint, address: int3)->float
    {
        return (material_words.Load(start + 72u) & 4u) != 0 ? mers_atlas.Load(address).y : 1;
    }
};
interface IEmitterCoverage
{
    static func accepts(start: uint, alpha: float)->bool;
};
struct OpaqueEmitter : IEmitterCoverage
{
    static func accepts(start: uint, alpha: float)->bool
    {
        return true;
    }
};
struct CutoutEmitter : IEmitterCoverage
{
    static func accepts(start: uint, alpha: float)->bool
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
public func sampled_material_emission(primitive: uint, bary: float2, scale: float)->float3
{
    let start : uint = material_id(primitive) * 112u;
    let address : int3 = sprite_address(material_uv(primitive, bary),
                                        asfloat(material_words.Load4(start + 80u)));
    let texel : float4 = atlas.Load(address);
    if (!EmitterCoverage.accepts(start, texel.a))
        return 0;
    let emission : float4 = asfloat(material_words.Load4(start + 48u));
    return emission.xyz * emission.w * (texel.rgb * quad_tint(primitive)) *
           EmissionWeight.read(start, address) * scale;
}
func emits_on_side(material: ModMaterial, front: bool)->bool
{
    let side : uint = front ? 1u : 2u;
    return (material.control.y & side) != 0 && material.emission.w > 0 &&
           ((material.control.z & 32u) == 0 || front);
}
/// AcceptedSample只用于已通过light_regions.sideMask与正PDF检查的forward样本。
/// reference/未知命中保留完整源规则，不套用仅覆盖已发布候选灯的上界。
public func material_emission<let accepted_sample : bool = false>(primitive: uint, bary: float2,
                                                                  front: bool,
                                                                  block_emission_scale: float)
    ->float3
{
    if (accepted_sample)
        return sampled_material_emission(primitive, bary, block_emission_scale);
    let material : ModMaterial = material_at(material_id(primitive));
    if (!emits_on_side(material, front))
        return 0;
    let uv : float2 = material_uv(primitive, bary);
    let texel : float4 = sample_tinted_texel(primitive, bary, uv);
    if ((material.control.z & 8u) != 0 && texel.a < 0.5)
        return 0;
    var result : float3 = material.emission.xyz * material.emission.w * texel.xyz;
    if ((material.control.z & 4u) != 0)
        result *= mers_atlas.Load(sprite_address(uv, material.uv_bounds)).y;
    return result * block_emission_scale;
}
func normal_sign(value: float2)->float2
{
    return float2(value.x >= 0 ? 1.0 : -1.0, value.y >= 0 ? 1.0 : -1.0);
}
public func encode_shading_normal(input_normal: float3)->uint
{
    var normal = input_normal;
    normal /= abs(normal.x) + abs(normal.y) + abs(normal.z);
    let oct : float2 = normal.z >= 0 ? normal.xy : (1.0 - abs(normal.yx)) * normal_sign(normal.xy);
    let value = uint2(round(saturate(oct * 0.5 + 0.5) * 65535.0));
    return value.x | (value.y << 16u);
}
public func decode_shading_normal(packed: uint)->float3
{
    let oct = float2(packed & 65535u, packed >> 16u) * (2.0 / 65535.0) - 1.0;
    var normal = float3(oct, 1.0 - abs(oct.x) - abs(oct.y));
    if (normal.z < 0)
        normal.xy = (1.0 - abs(normal.yx)) * normal_sign(normal.xy);
    return normalize(normal);
}
/// 只有显式法线贴图需要变换三角形边；几何法线和offset原样保留。
public func map_normal_xy(primitive: uint, encoded: float2, local_e1: float3, local_e2: float3,
                          object_to_world: float3x3, normal: float3)
    ->uint
{
    let base : uint = (primitive >> 1) * 3u;
    let uv01 : float4 = asfloat(quads[base]);
    let uv23 : float4 = asfloat(quads[base + 1u]);
    let second : bool = (primitive & 1u) != 0;
    let du : float2 = (second ? uv23.xy : uv01.zw) - uv01.xy;
    let dv : float2 = (second ? uv23.zw : uv23.xy) - uv01.xy;
    let determinant : float = du.x * dv.y - du.y * dv.x;
    if (determinant == 0 || !isfinite(determinant))
        return encode_shading_normal(normal);
    let e1 : float3 = mul(object_to_world, local_e1);
    let e2 : float3 = mul(object_to_world, local_e2);
    var tangent : float3 = (e1 * dv.y - e2 * du.y) / determinant;
    var bitangent : float3 = (e2 * du.x - e1 * dv.x) / determinant;
    tangent -= normal * dot(normal, tangent);
    let length_squared : float = dot(tangent, tangent);
    if (!(length_squared > 0) || !isfinite(length_squared))
        return encode_shading_normal(normal);
    tangent *= rsqrt(length_squared);
    let perpendicular = cross(normal, tangent);
    bitangent = perpendicular * (dot(perpendicular, bitangent) < 0 ? -1.0 : 1.0);
    let xy = fma(encoded, 255.0 / 127.0, -128.0 / 127.0);
    let local = float3(xy, sqrt(max(0.0, 1.0 - dot(xy, xy))));
    // oct编码消去正比例缩放；local和world的L2单位化均由最终L1归一化吸收。
    return encode_shading_normal(tangent * local.x + bitangent * local.y + normal * local.z);
}
public func mapped_shading_normal_known(primitive: uint, bary: float2, local_e1: float3,
                                        local_e2: float3, object_to_world: float3x3, normal: float3)
    ->uint
{
    return map_normal_xy(primitive, material_aux(primitive, bary).xy, local_e1, local_e2,
                         object_to_world, normal);
}
/// 未知命中的独立导引路径；照明叶子按资源期能力直接调用known版本。
public func mapped_shading_normal(primitive: uint, bary: float2, local_e1: float3, local_e2: float3,
                                  object_to_world: float3x3, normal: float3)
    ->uint
{
    if ((material_words.Load(material_id(primitive) * 112u + 76u) & 1u) == 0)
        return 0;
    return mapped_shading_normal_known(primitive, bary, local_e1, local_e2, object_to_world,
                                       normal);
}
// 能力位与 materials::variant 同步。通用 guide 根据当前命中读取相同信息。
func material_variant(material: ModMaterial)->uint
{
    let flags : uint = material.control.z;
    let sides : uint = material.control.y & ((flags & 32u) != 0 ? 1u : 3u);
    let emission : uint = material.emission.w > 0 ? (sides << 4u) | (flags & 4u) : 0;
    return (flags & 3u) | ((material.control.w & 1u) << 3u) | emission |
           ((material.control.x & 3u) << 6u) | ((flags & material_overlay_flags) << 1u) |
           ((flags & 512u) << 7u);
}

/// shaderLABS LabPBR 1.3 规定介质的绿色通道为线性 F0。金属的界面不进入介质栈。
public func lab_ior(encoded: float)->float
{
    let value = uint(round(saturate(encoded) * 255));
    if (value >= 230u)
        return 1.5;
    let amplitude = sqrt(float(value) / 255);
    return (1 + amplitude) / (1 - amplitude);
}

/// 由规范所列 n、k 的正入射复 Fresnel 反射率计算，底色负责给反射着色。
public func lab_metal_tint(encoded: float)->float3
{
    static const float3 reflectance[8] = {
        float3(0.5312288, 0.5123572, 0.4958285), float3(0.9442300, 0.7761021, 0.3734020),
        float3(0.9122980, 0.9138506, 0.9196806), float3(0.5555968, 0.5545371, 0.5547794),
        float3(0.9259522, 0.7209016, 0.5041542), float3(0.6324838, 0.6259371, 0.6414789),
        float3(0.6788492, 0.6424006, 0.5884096), float3(0.9620000, 0.9494681, 0.9221157)
    };
    let index = uint(round(saturate(encoded) * 255)) - 230u;
    return index < 8u ? reflectance[min(index, 7u)] : float3(1);
}

/// 消费已通过覆盖测试的命中；callable 的 Flow 和 variant 都在资源准备时特化。
public func resolve_material<let has_aux : bool = false, let read_color : bool = false>(
    input_hit: pt::Hit, flow: uint, variant: uint, block_emission_scale: float,
    sampled_aux: float4 = float4(0))
    ->transport::SurfaceMaterial
{
    var hit = input_hit;
    let primitive : uint = hit.identity.x;
    let material : ModMaterial = material_at(material_id(primitive));
    let mask : uint = variant == ~0u ? material_variant(material) : variant;
    var result : transport::SurfaceMaterial = {};
    result.pbr.base_roughness.w = material.base_roughness.w;
    result.pbr.surface = material.surface;
    result.pbr.control.x = (mask >> 6u) & 3u;
    let emits : bool = (flow & openpbr::FLOW_EMISSION) != 0 &&
                       ((mask & 48u) == 48u || ((mask & 16u) != 0 && hit.front) ||
                        ((mask & 32u) != 0 && !hit.front));
    let textured_emission : bool = emits && (mask & 4u) != 0;
    var uv : float2 = 0;
    let use_color : bool = reads_scattering_color(flow, mask);
    let overlay : bool =
                      (mask & variant_overlay) != 0 &&
                      (use_color || (mask & (1u | variant_overlay_mers)) != 0 || textured_emission);
    let read_base : bool = read_color && (mask & variant_overlay) == 0 && (use_color || emits);
    if (read_base || overlay || (mask & 3u) != 0 || textured_emission)
        uv = material_uv(primitive, asfloat(hit.identity.yz));
    var bounds : float4 = material.uv_bounds;
    if (read_base)
        hit.texel.xyz = atlas.Load(sprite_address(uv, bounds)).rgb * quad_tint(primitive);
    var base_color : float3 = hit.texel.xyz;
    var reads_mers : bool = (mask & 1u) != 0;
    if (overlay)
    {
        let local : float2 = (uv - material.uv_bounds.xy) /
                             (material.uv_bounds.zw - material.uv_bounds.xy);
        let top_uv : float2 = lerp(material.overlay_bounds.xy, material.overlay_bounds.zw, local);
        let top : float4 = atlas.Load(sprite_address(top_uv, material.overlay_bounds));
        if (top.a >= 0.5)
        {
            base_color = top.rgb * quad_tint(primitive);
            reads_mers = (mask & variant_overlay_mers) != 0;
            uv = top_uv;
            bounds = material.overlay_bounds;
        }
        else
        {
            base_color = atlas.Load(sprite_address(uv, bounds)).rgb;
        }
    }
    if (use_color)
        result.pbr.base_roughness.xyz = base_color * material.base_roughness.xyz;
    var mers = float4(material.surface.y, 0, material.base_roughness.w, material.surface.z);
    if (reads_mers || textured_emission)
        mers = mers_atlas.Load(sprite_address(uv, bounds));
    if (reads_mers)
    {
        result.pbr.base_roughness.w = mers.z;
        result.pbr.surface.y = mers.x;
        result.pbr.surface.z =
            (material.control.z & 512u) != 0 && (material.control.z & 96u) == 0 ? 0 : mers.w;
    }
    var medium : float4 = material.extinction;
    if ((mask & 2u) != 0)
    {
        let auxiliary : float4 = has_aux ? sampled_aux : aux_atlas.Load(sprite_address(uv, bounds));
        let lab = (mask & 65536u) != 0;
        let ior = lab ? lab_ior(auxiliary.w) : 1.0 + 1.5 * auxiliary.w;
        if (lab)
            result.pbr.base_roughness.xyz *= lab_metal_tint(auxiliary.w);
        result.pbr.surface.x = ior;
        medium.x = ior;
        if ((mask & 8u) != 0)
        {
            let alpha : float = result.pbr.base_roughness.w * result.pbr.base_roughness.w;
            let distribution : float = auxiliary.z * auxiliary.z;
            result.pbr.base_roughness.w =
                sqrt(sqrt(min(alpha * alpha + distribution * distribution, 1.0)));
            result.normal = decode_shading_normal(hit.identity.w);
        }
    }
    let thin : bool = (mask & 128u) != 0;
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
