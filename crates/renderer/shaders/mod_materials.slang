/// 资源世代的材质表、真实quad UV/tint与PBR纹理；不查询宿主或拥有GPU资源。
module mod_materials;
import pt.scene;
import bsdf.openpbr.api;

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
/// 保留旧共享tint的FP32插值顺序；覆盖alpha不乘tint。
public float4 material_texel(uint primitive, float2 barycentrics)
{
    float3 bary = float3(1.0 - barycentrics.x - barycentrics.y, barycentrics);
    float3 shared_tint = asfloat(quads[(primitive >> 1) * 3u + 2u].xyz);
    float3 tint = 0;
    tint += shared_tint * bary.x;
    tint += shared_tint * bary.y;
    tint += shared_tint * bary.z;
    float4 value = atlas.SampleLevel(atlas_sampler, material_uv(primitive, barycentrics), 0);
    return float4(value.xyz * material_linear(saturate(tint)), value.w);
}
/// CUTOUT来自实际源layer；opaque/透射界面不将alpha误解释为二值孔洞。
public bool material_accepts(uint primitive, float2 bary, bool front)
{
    ModMaterial material = materials[material_id(primitive)];
    if ((material.control.z & 32u) != 0 && !front)
        return false;
    return (material.control.z & 8u) == 0 || material_texel(primitive, bary).a >= 0.5;
}
public float3 material_emission(uint primitive, float2 bary, bool front)
{
    ModMaterial material = materials[material_id(primitive)];
    uint side = front ? 1u : 2u;
    if ((material.control.y & side) == 0 || material.emission.w <= 0 ||
        !material_accepts(primitive, bary, front))
        return 0;
    float3 result =
        material.emission.xyz * material.emission.w * material_texel(primitive, bary).xyz;
    if ((material.control.z & 4u) != 0)
        result *= mers_atlas.SampleLevel(atlas_sampler, material_uv(primitive, bary), 0).y;
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
/// e1/e2为实际三角形的world边；几何法线和offset原样保留，只编码独立着色法线。
public uint mapped_shading_normal(uint primitive, float2 bary, float3 e1, float3 e2, float3 normal)
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
public pt::SurfaceMaterial resolve_material(pt::Hit hit, float4 incident)
{
    uint primitive = hit.identity.x;
    ModMaterial material = materials[material_id(primitive)];
    pt::SurfaceMaterial result = {};
    result.pbr.base_roughness =
        float4(hit.texel.xyz * material.base_roughness.xyz, material.base_roughness.w);
    result.pbr.surface = material.surface;
    result.pbr.control.x = material.control.x & 3u;
    float2 uv = material_uv(primitive, asfloat(hit.identity.yz));
    float4 mers = float4(material.surface.y, 0, material.base_roughness.w, material.surface.z);
    if ((material.control.z & 1u) != 0)
    {
        mers = mers_atlas.SampleLevel(atlas_sampler, uv, 0);
        result.pbr.base_roughness.w = mers.z;
        result.pbr.surface.y = mers.x;
        result.pbr.surface.z = mers.w;
    }
    float4 medium = material.extinction;
    if ((material.control.z & 2u) != 0)
    {
        float4 auxiliary = aux_atlas.SampleLevel(atlas_sampler, uv, 0);
        float ior = 1.0 + 1.5 * (round(saturate(auxiliary.w) * 255.0) / 255.0);
        result.pbr.surface.x = ior;
        medium.x = ior;
        if ((material.control.w & 1u) != 0)
        {
            float alpha = result.pbr.base_roughness.w * result.pbr.base_roughness.w;
            float distribution = auxiliary.z * auxiliary.z;
            result.pbr.base_roughness.w =
                sqrt(sqrt(min(alpha * alpha + distribution * distribution, 1.0)));
            result.normal = decode_shading_normal(hit.identity.w);
        }
    }
    bool thin = (material.control.x & 2u) != 0;
    result.transmitted = thin || hit.front ? medium : float4(1, 0, 0, 0);
    result.emission = material_emission(primitive, asfloat(hit.identity.yz), hit.front);
    result.boundary_filter = 1;
    return result;
}
