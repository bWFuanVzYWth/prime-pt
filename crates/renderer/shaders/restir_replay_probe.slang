/// 诊断生产储层的恒等重放；只读已提交的路径，不改变采样或历史。
module restir_replay_probe;
import restir_scene;
import mod_scene;
import params;
import restir_pt.scene;
import restir_pt.types;
import restir_pt.shift;
import restir_pt.replay;
import restir_pt.storage;
import restir_pt.schedule;

[[vk::binding(39, 0)]]
RWByteAddressBuffer probe_paths;
[[vk::binding(40, 0)]]
StructuredBuffer<uint4> probe_primary;
[[vk::binding(47, 0)]]
RWStructuredBuffer<float4> probe_output;

#ifndef RESTIR_PROBE_STRIDE
#define RESTIR_PROBE_STRIDE 1
#endif

[shader("raygeneration")]
void restir_replay_probe()
{
    uint2 pixel = DispatchRaysIndex().xy * RESTIR_PROBE_STRIDE;
    if (any(pixel >= parameters[0].image.xy))
        return;
    uint index =
        restir_pt::reservoir_offset(pixel, restir_pt::screen_tiles(parameters[0].image.xy));
    uint probeWidth = (parameters[0].image.x + RESTIR_PROBE_STRIDE - 1) / RESTIR_PROBE_STRIDE;
    uint base = (DispatchRaysIndex().y * probeWidth + DispatchRaysIndex().x) * 5;
    restir_pt::PathReservoir source = restir_pt::load_reservoir(probe_paths, index);
    probe_output[base] = float4(source.integrand, source.weight);
    probe_output[base + 1] = 0;
    probe_output[base + 2] = asfloat(
        uint4(source.pathFlags.flags, source.initRandomSeed, source.reconnectionHit.data.xy));
    probe_output[base + 3] =
        float4(source.reconnectionIncidentRadiance, source.reconnectionJacobian);
    probe_output[base + 4] = asfloat(source.reconnectionHit.data);
    restir_pt::HitInfo hit = { probe_primary[index] };
    if (!hit.isValid() || source.weight == 0)
        return;
    RestirScene scene = {};
    restir_pt::Surface surface =
        scene.loadSurfaceFromOrigin(hit, scene.initialOrigin(pixel), false);
    surface.wo = -scene.initialDirection(pixel);
    scene.setIncident(surface, scene.initialMedium(pixel));
#if RESTIR_PREVIOUS_PRIMARY_PROBE
    surface = scene.loadPreviousPrimary(hit, pixel);
#endif
#if RESTIR_ROUNDTRIP_PROBE
    uint2 neighbor = pixel ^ uint2(7, 7);
    if (any(neighbor >= parameters[0].image.xy))
    {
        probe_output[base].w = 0;
        return;
    }
    uint other =
        restir_pt::reservoir_offset(neighbor, restir_pt::screen_tiles(parameters[0].image.xy));
    restir_pt::HitInfo otherHit = { probe_primary[other] };
    if (!otherHit.isValid())
    {
        probe_output[base].w = 0;
        return;
    }
    restir_pt::Surface destination =
        scene.loadSurfaceFromOrigin(otherHit, scene.initialOrigin(neighbor), false);
    destination.wo = -scene.initialDirection(neighbor);
    scene.setIncident(destination, scene.initialMedium(neighbor));
    float forwardJacobian;
    float3 forward = restir_pt::shiftPath(
        scene, destination, source, scene.primaryRayFootprint(destination, neighbor, false),
        restir_pt::ShiftSettings(), true, true, true, false, forwardJacobian);
    if (forwardJacobian <= 0 || all(forward == 0))
    {
        probe_output[base].w = 0;
        return;
    }
#endif
    float jacobian;
    float3 value = restir_pt::shiftPath(
        scene, surface, source, scene.primaryRayFootprint(surface, pixel, false),
        restir_pt::ShiftSettings(), true, true, true, false, jacobian);
#if RESTIR_ROUNDTRIP_PROBE
    jacobian *= forwardJacobian;
#endif
    probe_output[base + 1] = float4(value, jacobian);
    if (source.pathFlags.rcVertexLength() == 1 && source.reconnectionHit.isValid() &&
        (jacobian <= 0 ||
         any(abs(value - probe_output[base].xyz) > 5e-5 + 3e-3 * abs(probe_output[base].xyz))))
    {
        restir_pt::Surface endpoint =
            scene.loadSurfaceFromOrigin(source.reconnectionHit, surface.point.position, false);
        float3 delta = endpoint.point.position - surface.point.position;
        float3 direction = normalize(delta);
        restir_pt::BsdfEvaluation first = scene.evaluateBsdf(surface, direction, -1);
        restir_pt::BsdfEvaluation second = scene.evaluateBsdf(endpoint, source.reconnectionWi, -1);
        float distanceThreshold, roughnessThreshold;
        restir_pt::initReplaySampler(source.initRandomSeed, restir_pt::ShiftSettings(),
                                     distanceThreshold, roughnessThreshold);
        float footprint = scene.primaryRayFootprint(surface, pixel, false) * distanceThreshold;
        float inverseGeometry = dot(delta, delta) / abs(dot(surface.point.normal, -direction));
        uint status = uint(scene.visible(surface, endpoint, false)) |
                      (uint(scene.isSampleRough(surface, 0, first.pdf, roughnessThreshold)) << 1);
        probe_output[base + 4] =
            float4(status, first.pdf, second.pdf, inverseGeometry / second.pdf - footprint);
    }
}
