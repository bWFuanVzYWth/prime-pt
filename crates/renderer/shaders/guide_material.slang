/// 材质键专用RR辅助输出；closest-hit和已知表面callable共用数值叶子，不计算照明。
module guide_material;
import pt.key_guides;
import pt.scene;
import mod_scene;
import restir_geometry;
import restir_pt.types;
import geometry.ray;

#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from resource preparation
#endif

pt::KeyGuideVertex guide_value(pt::Hit hit, float3 incoming, float4 incident, float4 auxiliary)
{
    ModScene provider;
    pt::SurfaceMaterial material = resolve_material<true, true>(
        hit, OPENPBR_FLOW, OPENPBR_VARIANT, parameters[0].block_emission_scale, auxiliary);
    return pt::make_key_guide<OPENPBR_FLOW>(hit, material, provider.energy(),
                                            provider.guide_pose(hit), incoming, incident);
}

[shader("closesthit")]
void guide_hit(inout pt::KeyGuideCall call, BuiltInTriangleIntersectionAttributes attributes)
{
    float3 a = HitTriangleVertexPosition(0), b = HitTriangleVertexPosition(1),
           c = HitTriangleVertexPosition(2);
    pt::Hit hit = {};
    hit.surface = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                           WorldToObject3x4());
    uint primitive = region_primitive(InstanceID(), PrimitiveIndex());
    float4 auxiliary = material_aux<OPENPBR_VARIANT>(primitive, attributes.barycentrics);
    uint normal = 0;
    if ((OPENPBR_VARIANT & 8u) != 0)
        normal = map_normal_xy(primitive, auxiliary.xy, b - a, c - a, (float3x3)ObjectToWorld3x4(),
                               hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = RayTCurrent();
    hit.front = dot(WorldRayDirection(), hit.surface.normal) < 0;
    call.value = guide_value(hit, WorldRayDirection(), call.value.medium, auxiliary);
}

[shader("callable")]
void guide_known(inout pt::KeyGuideCall call)
{
    restir_pt::HitInfo identity = { asuint(
        float4(call.value.point.position, call.value.distance)) };
    float3 incoming = call.value.canonicalDirection;
    pt::Hit hit = {};
    hit.identity = uint4(restir_primitive(identity), identity.data.zw, 0);
    hit.surface =
        geometry::surface_point(restir_endpoint(hit.identity.x, asfloat(identity.data.zw)));
    float4 auxiliary = material_aux<OPENPBR_VARIANT>(hit.identity.x, asfloat(hit.identity.yz));
    hit.identity.w =
        restir_normal_xy<OPENPBR_VARIANT>(hit.identity.x, auxiliary.xy, hit.surface.normal);
    hit.front = dot(incoming, hit.surface.normal) < 0;
    hit.distance = length(hit.surface.position - parameters[0].origin.xyz);
    call.value = guide_value(hit, incoming, call.value.medium, auxiliary);
}
