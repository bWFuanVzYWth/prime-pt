/// 稳定几何目录与已知表面的误差界恢复；法线贴图仅由专用叶子按静态能力读取。
module restir_geometry;
import mod_scene;
import geometry.identity;
import restir_pt.types;
import pt.light_sampling;
import geometry.ray;
[[vk::binding(37, 0)]]
StructuredBuffer<uint4> restir_positions;

/// 头部后的quad数据与容量尾部的活动域目录在同一事务发布。
uint4 restir_domain(uint token)
{
    uint base = restir_positions[0].y;
    uint index = geometry::identity_index(restir_positions, base, token);
    return index == 0xffffffffu ? uint4(0) : restir_positions[base + 1u + index];
}
public uint restir_primitive(restir_pt::HitInfo hit)
{
    if (!hit.isValid())
        return 0xffffffffu;
    uint4 domain = restir_domain(hit.data.x);
    return domain.w == hit.data.x && (hit.data.y >> 1u) < domain.y ? domain.x * 2u + hit.data.y
                                                                   : 0xffffffffu;
}
public bool restir_valid_hit(restir_pt::HitInfo hit)
{
    return restir_primitive(hit) != 0xffffffffu;
}
public restir_pt::HitInfo restir_stable_hit(uint primitive, float2 bary)
{
    uint token = restir_positions[1u + (primitive >> 1u) * 4u + 3u].w;
    uint4 domain = restir_domain(token);
    if (token == 0u || domain.w != token)
        return {};
    return { uint4(token, primitive - domain.x * 2u, asuint(bary)) };
}

public uint restir_normal_xy<let Variant : uint>(uint primitive, float2 encoded, float3 normal)
{
    if ((Variant & 8u) == 0)
        return 0;
    uint start = 1u + (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(asfloat(restir_positions[start]),
                                                             asfloat(restir_positions[start + 1u]),
                                                             asfloat(restir_positions[start + 2u]));
    bool second = (primitive & 1u) != 0;
    return map_normal_xy(primitive, encoded, (second ? vertices.c : vertices.b) - vertices.a,
                         (second ? vertices.d : vertices.c) - vertices.a,
                         float3x3(1, 0, 0, 0, 1, 0, 0, 0, 1), normal);
}

public uint restir_normal<let Variant : uint>(uint primitive, float2 bary, float3 normal)
{
    if ((Variant & 8u) == 0)
        return 0;
    return restir_normal_xy<Variant>(primitive, material_aux(primitive, bary).xy, normal);
}

public geometry::TriangleEndpoint restir_endpoint(uint primitive, float2 bary)
{
    uint start = 1u + (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(asfloat(restir_positions[start]),
                                                             asfloat(restir_positions[start + 1u]),
                                                             asfloat(restir_positions[start + 2u]));
    pt::QuadAreaSample sample = { primitive & 1u, bary, 0 };
    float3 translation =
        float3(asint(restir_positions[start + 3u].xyz) - parameters[0].anchor_region.xyz) * 64.0;
    float3x4 transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    float3x4 inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    return pt::quad_endpoint(vertices, sample, transform, inverse);
}

public geometry::TriangleEndpoint restir_endpoint(restir_pt::HitInfo identity)
{
    return restir_endpoint(restir_primitive(identity), asfloat(identity.data.zw));
}
