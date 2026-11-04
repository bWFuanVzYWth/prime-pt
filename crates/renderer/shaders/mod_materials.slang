/// 资源世代的材质表、真实quad UV/tint与PBR纹理；不查询宿主或拥有GPU资源。
module mod_materials;
import pt.scene;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;

#ifndef PRIMEPT_COVERAGE_MASK
#define PRIMEPT_COVERAGE_MASK 40u
#endif
public static const uint COVERAGE_MASK = PRIMEPT_COVERAGE_MASK;

[[vk::binding(1, 0)]]
public StructuredBuffer<uint4> quads;
[[vk::binding(2, 0)]]
public Texture2D<float4> atlas;
[[vk::binding(3, 0)]]
public SamplerState atlas_sampler;

/// GPU派生的96B六块表；物理字段与prime_scene::MaterialDefinition保持同位。
/// control.x低2位为源OpenPBR flags，高30位由资源准备写入callable槽，不回写CPU定义。
public struct ModMaterial
{
    public float4 base_roughness;
    public float4 surface;
    public float4 extinction;
    public float4 emission;
    public uint4 control;
    public float4 uv_bounds;
};
[[vk::binding(27, 0)]]
public StructuredBuffer<ModMaterial> materials;
[[vk::binding(28, 0)]]
Texture2D<float4> mers_atlas;
[[vk::binding(29, 0)]]
Texture2D<float4> aux_atlas;

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
float4 sample_tinted_texel(uint primitive, float2 barycentrics, float2 uv)
{
    float3 bary = float3(1.0 - barycentrics.x - barycentrics.y, barycentrics);
    float3 shared_tint = asfloat(quads[(primitive >> 1) * 3u + 2u].xyz);
    float3 tint = 0;
    tint += shared_tint * bary.x;
    tint += shared_tint * bary.y;
    tint += shared_tint * bary.z;
    float4 value = atlas.SampleLevel(atlas_sampler, uv, 0);
    return float4(value.xyz * material_linear(saturate(tint)), value.w);
}
/// 保留旧共享tint的FP32插值顺序；覆盖alpha不乘tint。
public float4 material_texel(uint primitive, float2 barycentrics)
{
    return sample_tinted_texel(primitive, barycentrics, material_uv(primitive, barycentrics));
}
/// CUTOUT来自实际源layer；opaque/透射界面不将alpha误解释为二值孔洞。
public bool material_accepts(uint primitive, float2 bary, bool front)
{
    uint flags = materials[material_id(primitive)].control.z & COVERAGE_MASK;
    if ((flags & 32u) != 0 && !front)
        return false;
    return (flags & 8u) == 0 ||
           atlas.SampleLevel(atlas_sampler, material_uv(primitive, bary), 0).a >= 0.5;
}
bool emits_on_side(ModMaterial material, bool front)
{
    uint side = front ? 1u : 2u;
    return (material.control.y & side) != 0 && material.emission.w > 0 &&
           ((material.control.z & 32u) == 0 || front);
}
public float3 material_emission(uint primitive, float2 bary, bool front)
{
    ModMaterial material = materials[material_id(primitive)];
    if (!emits_on_side(material, front))
        return 0;
    float2 uv = material_uv(primitive, bary);
    float4 texel = sample_tinted_texel(primitive, bary, uv);
    if ((material.control.z & 8u) != 0 && texel.a < 0.5)
        return 0;
    float3 result = material.emission.xyz * material.emission.w * texel.xyz;
    if ((material.control.z & 4u) != 0)
        result *= mers_atlas.SampleLevel(atlas_sampler, uv, 0).y;
    return result;
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
public uint mapped_shading_normal(uint primitive, float2 bary, float3 local_e1, float3 local_e2,
                                  float3x3 object_to_world, float3 normal)
{
    ModMaterial material = materials[material_id(primitive)];
    if ((material.control.w & 1u) == 0)
        return 0;
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
    float2 xy =
        (aux_atlas.SampleLevel(atlas_sampler, material_uv(primitive, bary), 0).xy * 255.0 - 128.0) /
        127.0;
    float3 local = normalize(float3(xy, sqrt(max(0.0, 1.0 - dot(xy, xy)))));
    return encode_shading_normal(
        normalize(tangent * local.x + bitangent * local.y + normal * local.z));
}
// 低8位与 materials::variant 同步。通用 guide 根据当前命中读取相同信息。
uint material_variant(ModMaterial material)
{
    uint flags = material.control.z;
    uint sides = (flags & 32u) != 0 ? material.control.y & 1u : material.control.y;
    uint emission = material.emission.w > 0 ? (sides << 4u) | (flags & 4u) : 0;
    return (flags & 3u) | ((material.control.w & 1u) << 3u) | emission |
           ((material.control.x & 3u) << 6u);
}

/// 消费已通过覆盖测试的命中；callable 的 Flow 和 variant 都在资源准备时特化。
public pt::SurfaceMaterial resolve_material(pt::Hit hit, uint flow, uint variant)
{
    uint primitive = hit.identity.x;
    ModMaterial material = materials[material_id(primitive)];
    uint mask = variant == ~0u ? material_variant(material) : variant;
    pt::SurfaceMaterial result = {};
    result.pbr.base_roughness.w = material.base_roughness.w;
    if ((flow & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                 openpbr::FLOW_CONDUCTOR_REFLECTION)) != 0)
        result.pbr.base_roughness.xyz = hit.texel.xyz * material.base_roughness.xyz;
    result.pbr.surface = material.surface;
    result.pbr.control.x = (mask >> 6u) & 3u;
    bool emits = (flow & openpbr::FLOW_EMISSION) != 0 &&
                 ((mask & 48u) == 48u || ((mask & 16u) != 0 && hit.front) ||
                  ((mask & 32u) != 0 && !hit.front));
    bool textured_emission = emits && (mask & 4u) != 0;
    float2 uv = 0;
    if ((mask & 3u) != 0 || textured_emission)
        uv = material_uv(primitive, asfloat(hit.identity.yz));
    float4 mers = float4(material.surface.y, 0, material.base_roughness.w, material.surface.z);
    if ((mask & 1u) != 0 || textured_emission)
        mers = mers_atlas.SampleLevel(atlas_sampler, uv, 0);
    if ((mask & 1u) != 0)
    {
        result.pbr.base_roughness.w = mers.z;
        result.pbr.surface.y = mers.x;
        result.pbr.surface.z = mers.w;
    }
    float4 medium = material.extinction;
    if ((mask & 2u) != 0)
    {
        float4 auxiliary = aux_atlas.SampleLevel(atlas_sampler, uv, 0);
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
    }
    result.boundary_filter = 1;
    return result;
}
