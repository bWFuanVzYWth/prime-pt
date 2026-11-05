/// 资源准备时按真实Flow、材质能力和操作编译；叶子没有追踪或随机生成。
module restir_material;
import mod_scene;
import restir_call;
import pt.scene;
import pt.transport;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;
import bsdf.openpbr.specialized;

#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from resource preparation
#endif
#ifndef PRIMEPT_RESTIR_SAMPLE
#define PRIMEPT_RESTIR_SAMPLE 1
#endif
#ifndef PRIMEPT_LIGHT_MASK
#define PRIMEPT_LIGHT_MASK 3
#endif
// 普通PT按太阳/局部灯排列；初采按局部灯/太阳排列，资源世代消除不存在的灯族求值。
static const uint INITIAL_LIGHT_MASK =
    ((PRIMEPT_LIGHT_MASK & 2u) >> 1u) | ((PRIMEPT_LIGHT_MASK & 1u) << 1u);

#if defined(PRIMEPT_INITIAL_HIT) && (defined(PRIMEPT_HIT_GEOMETRY) || defined(PRIMEPT_HIT_GEOMETRY_LEAF))
RestirMaterialHit traced_material_hit(BuiltInTriangleIntersectionAttributes attributes)
{
    float3 a = HitTriangleVertexPosition(0), b = HitTriangleVertexPosition(1),
           c = HitTriangleVertexPosition(2);
    geometry::SurfacePoint point = geometry::triangle_point(a, b, c, attributes.barycentrics,
                                                            ObjectToWorld3x4(), WorldToObject3x4());
    uint primitive = region_primitive(InstanceID(), PrimitiveIndex());
    uint normal = 0;
    if ((OPENPBR_VARIANT & 8u) != 0)
        normal = mapped_shading_normal(primitive, attributes.barycentrics, b - a, c - a,
                                       (float3x3)ObjectToWorld3x4(), point.normal);
    return { point.normal, uint4(primitive, asuint(attributes.barycentrics), normal),
             uint(dot(WorldRayDirection(), point.normal) < 0) };
}
#endif

#ifdef PRIMEPT_INITIAL_HIT
[shader("closesthit")]
#else
[shader("callable")]
#endif
#if PRIMEPT_RESTIR_SAMPLE
void restir_initial_flow(inout RestirInitialCall call
#ifdef PRIMEPT_INITIAL_HIT
                         ,
                         BuiltInTriangleIntersectionAttributes attributes
#endif
)
{
#if defined(PRIMEPT_INITIAL_HIT) && (defined(PRIMEPT_HIT_GEOMETRY) || defined(PRIMEPT_HIT_GEOMETRY_LEAF))
    call.hit = traced_material_hit(attributes);
    call.wo = -WorldRayDirection();
#endif
    pt::Hit hit = to_pt_hit(call.hit);
    ModScene scene;
    pt::SurfaceMaterial material = scene.resolve_flow(hit, OPENPBR_FLOW, call.incident);
    float3 normal = pt::shading_normal(hit, material);
    openpbr::Batch batch = openpbr::shade_flow<OPENPBR_FLOW, openpbr::Energy, INITIAL_LIGHT_MASK>(
        scene.energy(), material.pbr, normal, call.wo, call.direction, call.second_direction,
        INITIAL_LIGHT_MASK, call.u, PRIMEPT_RESTIR_SAMPLE != 0, call.incident,
        material.transmitted);
    store_sample(call.hit, call.incident, batch.scattered);
    call.wo = batch.first.response;
    call.u.x = batch.first.pdf;
    call.direction = batch.second.response;
    call.u.y = batch.second.pdf;
    call.second_direction = material.emission;
}
#else
void restir_initial_flow(inout RestirDirectCall call
#ifdef PRIMEPT_INITIAL_HIT
                         ,
                         BuiltInTriangleIntersectionAttributes attributes
#endif
)
{
#if defined(PRIMEPT_INITIAL_HIT) && (defined(PRIMEPT_HIT_GEOMETRY) || defined(PRIMEPT_HIT_GEOMETRY_LEAF))
    call.hit = traced_material_hit(attributes);
    call.wo = -WorldRayDirection();
#endif
    ModScene scene;
    pt::Hit hit = to_pt_hit(call.hit);
    float4 incident = float4(call.incident_ior, 0, 0, 0);
    pt::SurfaceMaterial material = scene.resolve_flow(hit, OPENPBR_FLOW, incident);
    openpbr::Batch batch = openpbr::shade_flow<OPENPBR_FLOW, openpbr::Energy, INITIAL_LIGHT_MASK>(
        scene.energy(), material.pbr, pt::shading_normal(hit, material), call.wo, call.direction,
        call.second_direction, INITIAL_LIGHT_MASK, float3(0), false, incident,
        material.transmitted);
    call.hit.identity = asuint(float4(batch.first.pdf, batch.first.response));
    call.hit.normal = material.emission;
    call.wo = batch.second.response;
    call.direction.x = batch.second.pdf;
}
#endif

[shader("callable")]
void restir_replay_flow(inout RestirReplayCall call)
{
    pt::Hit hit = to_pt_hit(call.hit);
    ModScene scene;
    pt::SurfaceMaterial material = scene.resolve_flow(hit, OPENPBR_FLOW, call.incident);
    openpbr::Sample sampled = openpbr::sample_flow<OPENPBR_FLOW>(
        scene.energy(), material.pbr, pt::shading_normal(hit, material), call.wo, call.u,
        call.incident, material.transmitted);
    store_sample(call.hit, call.incident, sampled);
}

[shader("callable")]
void restir_evaluate_flow(inout RestirEvaluateCall call)
{
    pt::Hit hit = to_pt_hit(call.hit);
    ModScene scene;
    float4 incident = float4(call.incident_ior, 0, 0, 0);
    pt::SurfaceMaterial material = scene.resolve_flow(hit, OPENPBR_FLOW, incident);
    openpbr::Evaluation evaluated = openpbr::evaluate_flow<OPENPBR_FLOW>(
        scene.energy(), material.pbr, pt::shading_normal(hit, material), call.wo, call.direction,
        incident, material.transmitted);
    call.hit.identity = asuint(float4(evaluated.pdf, evaluated.response));
}
