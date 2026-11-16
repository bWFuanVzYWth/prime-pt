/// 已知首表面的误差界恢复；法线贴图仅由专用叶子按静态能力读取。
module known_geometry;
import mod_scene;
import pt.light_sampling;
import geometry.ray;
[[vk::binding(39, 0)]]
var known_positions : StructuredBuffer<uint4>;

public func known_normal_xy<let variant : uint>(primitive: uint, encoded: float2, normal: float3)
    ->uint
{
    if ((variant & 8u) == 0)
        return 0;
    let start : uint = (primitive >> 1u) * 4u;
    let vertices : pt::LightQuadVertices = { asfloat(known_positions[start].xyz),
                                             asfloat(known_positions[start + 1u].xyz),
                                             asfloat(known_positions[start + 2u].xyz),
                                             asfloat(known_positions[start + 3u].xyz) };
    let second : bool = (primitive & 1u) != 0;
    return map_normal_xy(primitive, encoded, (second ? vertices.c : vertices.b) - vertices.a,
                         (second ? vertices.d : vertices.c) - vertices.a,
                         float3x3(1, 0, 0, 0, 1, 0, 0, 0, 1), normal);
}

public func known_normal<let variant : uint>(primitive: uint, bary: float2, normal: float3)->uint
{
    if ((variant & 8u) == 0)
        return 0;
    return known_normal_xy<variant>(primitive, material_aux(primitive, bary).xy, normal);
}

public func known_endpoint(primitive: uint, bary: float2)->geometry::TriangleEndpoint
{
    let start : uint = (primitive >> 1u) * 4u;
    let vertices : pt::LightQuadVertices = { asfloat(known_positions[start].xyz),
                                             asfloat(known_positions[start + 1u].xyz),
                                             asfloat(known_positions[start + 2u].xyz),
                                             asfloat(known_positions[start + 3u].xyz) };
    let random_sample : pt::QuadAreaSample = { primitive & 1u, bary, 0 };
    let translation =
        float3(int3(asint(known_positions[start].w), asint(known_positions[start + 1u].w),
                    asint(known_positions[start + 2u].w)) -
               parameters[0].anchor_region.xyz) *
        64.0;
    let transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    let inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    return pt::quad_endpoint(vertices, random_sample, transform, inverse);
}
