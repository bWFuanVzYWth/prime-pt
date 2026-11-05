/// 资源准备时按真实Flow、材质能力和操作编译；叶子没有追踪或随机生成。
module restir_material;
import mod_scene;
import restir_call;
import pt.scene;
import pt.transport;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;
import bsdf.openpbr.specialized;

#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from resource preparation
#endif
#ifndef PRIMEPT_RESTIR_SAMPLE
#define PRIMEPT_RESTIR_SAMPLE 1
#endif

[shader("callable")]
void restir_initial_flow(inout RestirInitialCall call)
{
    pt::Hit hit = to_pt_hit(call.hit);
    ModScene scene;
    pt::SurfaceMaterial material = scene.resolve_flow(hit, OPENPBR_FLOW, call.incident);
    float3 normal = pt::shading_normal(hit, material);
    openpbr::Batch batch = openpbr::shade_flow<OPENPBR_FLOW, openpbr::Energy, 1>(
        scene.energy(), material.pbr, normal, call.wo, call.direction, float3(0), 1u, call.u,
        PRIMEPT_RESTIR_SAMPLE != 0, call.incident, material.transmitted);
    store_sample(call.hit, call.incident, batch.scattered);
    call.wo = batch.first.response;
    call.u.x = batch.first.pdf;
    call.direction = material.emission;
}

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
    pt::SurfaceMaterial material = scene.resolve_flow(hit, OPENPBR_FLOW, call.incident);
    openpbr::Evaluation evaluated = openpbr::evaluate_flow<OPENPBR_FLOW>(
        scene.energy(), material.pbr, pt::shading_normal(hit, material), call.wo, call.direction,
        call.incident, material.transmitted);
    call.hit.texel = float4(evaluated.response, evaluated.pdf);
}
