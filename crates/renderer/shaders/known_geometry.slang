/// 已知首表面的误差界恢复；法线贴图仅由专用叶子按静态能力读取。
module known_geometry;
import mod_scene;
import pt.light_sampling;
import geometry.ray;
[[vk::binding(37, 0)]]
StructuredBuffer<uint4> known_positions;

public uint known_normal_xy<let Variant : uint>(uint primitive, float2 encoded, float3 normal)
{
    if ((Variant & 8u) == 0)
        return 0;
    uint start = 1u + (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(asfloat(known_positions[start]),
                                                             asfloat(known_positions[start + 1u]),
                                                             asfloat(known_positions[start + 2u]));
    bool second = (primitive & 1u) != 0;
    return map_normal_xy(primitive, encoded, (second ? vertices.c : vertices.b) - vertices.a,
                         (second ? vertices.d : vertices.c) - vertices.a,
                         float3x3(1, 0, 0, 0, 1, 0, 0, 0, 1), normal);
}

public uint known_normal<let Variant : uint>(uint primitive, float2 bary, float3 normal)
{
    if ((Variant & 8u) == 0)
        return 0;
    return known_normal_xy<Variant>(primitive, material_aux(primitive, bary).xy, normal);
}

public geometry::TriangleEndpoint known_endpoint(uint primitive, float2 bary)
{
    uint start = 1u + (primitive >> 1u) * 4u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(asfloat(known_positions[start]),
                                                             asfloat(known_positions[start + 1u]),
                                                             asfloat(known_positions[start + 2u]));
    pt::QuadAreaSample sample = { primitive & 1u, bary, 0 };
    float3 translation =
        float3(asint(known_positions[start + 3u].xyz) - parameters[0].anchor_region.xyz) * 64.0;
    float3x4 transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    float3x4 inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    return pt::quad_endpoint(vertices, sample, transform, inverse);
}
