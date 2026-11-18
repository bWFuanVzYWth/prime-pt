/// 材质键专用RR辅助输出；closest-hit和已知表面callable共用数值叶子，不计算照明。
module guide_material;
import pt.key_guides;
import pt.scene;
import mod_scene;
import mod_decode;
import known_geometry;
import geometry.ray;
import primary_cache;
import transport.surface_material;

#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from resource preparation
#endif

func guide_value(hit: pt::Hit, incoming: float3, incident: float4, auxiliary: float4)
    ->pt::KeyGuideVertex
{
    var provider : ModScene;
    let material : transport::SurfaceMaterial = decode_surface<AtlasColor, CachedAux>(
                       hit, parameters[0].block_emission_scale, { auxiliary });
    return pt::make_key_guide<OPENPBR_FLOW>(hit, material, provider.energy(),
                                            provider.guide_pose(hit), incoming, incident);
}

[shader("closesthit")]
func guide_hit(inout call: pt::KeyGuideCall, attributes: BuiltInTriangleIntersectionAttributes)
    ->void
{
    let a : float3 = HitTriangleVertexPosition(0);
    let b : float3 = HitTriangleVertexPosition(1);
    let c : float3 = HitTriangleVertexPosition(2);
    var hit : pt::Hit = {};
    hit.surface = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                           WorldToObject3x4());
    let primitive : uint = region_primitive(InstanceID(), PrimitiveIndex());
    let auxiliary : float4 = material_aux<OPENPBR_VARIANT>(primitive, attributes.barycentrics);
    var normal : uint = 0;
    if ((OPENPBR_VARIANT & 8u) != 0)
        normal = map_normal_xy(primitive, auxiliary.xy, b - a, c - a, (float3x3)ObjectToWorld3x4(),
                               hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = RayTCurrent();
    hit.front = dot(WorldRayDirection(), hit.surface.normal) < 0;
    call.value = guide_value(hit, WorldRayDirection(), call.value.medium, auxiliary);
}

// 首命中来自第一阶段，不再追相机射线；delta后缀仍独立重追。
[shader("callable")]
func guide_primary(inout call: pt::KeyGuideCall)->void
{
    let pixel : uint = primary_pixel();
    var hit : pt::Hit = {};
    hit.identity = primary_identity[pixel];
    hit.surface = geometry::surface_point(known_endpoint(hit.identity.x, asfloat(hit.identity.yz)));
    let ray : float4 = primary_ray[pixel];
    let auxiliary : float4 =
                        material_aux<OPENPBR_VARIANT>(hit.identity.x, asfloat(hit.identity.yz));
    hit.identity.w =
        known_normal_xy<OPENPBR_VARIANT>(hit.identity.x, auxiliary.xy, hit.surface.normal);
    hit.front = dot(ray.xyz, hit.surface.normal) < 0;
    hit.distance = ray.w;
    call.value = guide_value(hit, ray.xyz, call.value.medium, auxiliary);
}
