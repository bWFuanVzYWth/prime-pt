/// 专门核对实际操作SBT路由；参考分支只用于此测试，不进入生产入口。
module material_probe;
import restir_scene;
import restir_call;
import restir_pt.scene;
import restir_pt.types;
import restir_pt.schedule;
import mod_scene;
import pt.scene;
import pt.key_guides;
import pt.denoise;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;
#ifndef PRIMEPT_PROBE_GUIDE_OFFSET
#error PRIMEPT_PROBE_GUIDE_OFFSET must come from the prepared SBT
#endif
[[vk::binding(40, 0)]]
StructuredBuffer<uint4> probe_primary;
[[vk::binding(47, 0)]]
RWStructuredBuffer<float4> probe_output;

[shader("raygeneration")]
void material_route_probe()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint base = (pixel.y * parameters[0].image.x + pixel.x) * 31u;
    for (uint i = 0; i < 31u; ++i)
        probe_output[base + i] = 0;
    restir_pt::HitInfo identity = { probe_primary[restir_pt::reservoir_offset(
        pixel, restir_pt::screen_tiles(parameters[0].image.xy))] };
    if (!restir_valid_hit(identity))
        return;
    RestirScene scene = {};
    float3 from = parameters[0].origin.xyz;
    restir_pt::Surface geometry = scene.loadGeometrySurfaceFromOrigin(identity, from);
    restir_pt::Surface actual = scene.loadSurfaceFromOrigin(identity, from, false);
    uint primitive = geometry.identity.x;
    ModMaterial source = material_at(material_id(primitive));
    pt::Hit referenceHit = restir_hit(geometry);
    if ((source.control.w & 1u) != 0)
        referenceHit.identity.w =
            restir_normal<8u>(primitive, asfloat(identity.data.zw), geometry.point.normal);
    referenceHit.texel = material_texel(primitive, asfloat(identity.data.zw));
    pt::SurfaceMaterial reference =
        resolve_material(referenceHit, ~0u, ~0u, parameters[0].block_emission_scale);
    float4 incident = geometry.frontFacing || (reference.pbr.control.x & 3u) != 1u
                          ? float4(1, 0, 0, 0)
                          : float4(reference.pbr.surface.x, source.extinction.yzw);
    probe_output[base] = float4(actual.roughness, asfloat(actual.material), actual.capability,
                                actual.hasNonDeltaLobes ? 1.0 : 0.0);
    probe_output[base + 1] = float4(
        reference.pbr.base_roughness.w, reference.pbr.surface.y, reference.pbr.control.x,
        openpbr::is_discrete_only(reference.pbr, incident, reference.transmitted) ? 0.0 : 1.0);
    probe_output[base + 2] = actual.transmitted;
    probe_output[base + 3] = reference.transmitted;
    probe_output[base + 4] = actual.incident;
    probe_output[base + 5] = incident;

    restir_pt::Surface endpoint = {};
    endpoint.hit = identity;
    endpoint.wo = geometry.wo;
    restir_pt::EmitterEvaluation emitted = scene.evaluateEmitter(endpoint, from, false);
    float referencePdf = ModScene().emission_pdf(referenceHit, from);
    probe_output[base + 6] = float4(emitted.emission, emitted.pdf);
    probe_output[base + 7] =
        float4(reference.emission, any(reference.emission > 0) ? referencePdf : 0.0);
    bool canEmit = ((source.control.z >> 20u) & openpbr::FLOW_EMISSION) != 0;
    probe_output[base + 8] = float4(emitted.position, 0);
    probe_output[base + 9] = float4(canEmit ? geometry.point.position : float3(0), 0);
    probe_output[base + 10] = float4(emitted.normal, 0);
    probe_output[base + 11] = float4(canEmit ? geometry.point.normal : float3(0), 0);
    restir_pt::Surface predecessor = {};
    predecessor.point.position = from;
    float actualPdf =
        scene.lightPdf(predecessor, identity, -geometry.wo, restir_pt::LIGHT_EMISSIVE, false);
    probe_output[base + 12] = float4(actualPdf, 0, 0, 0);
    probe_output[base + 13] = float4(referencePdf, 0, 0, 0);

    pt::KeyGuideCall guide = {};
    guide.value.point.position = asfloat(identity.data.xyz);
    guide.value.distance = asfloat(identity.data.w);
    guide.value.canonicalDirection = -geometry.wo;
    guide.value.medium = incident;
    CallShader(PRIMEPT_PROBE_GUIDE_OFFSET + material_program(primitive, MATERIAL_GUIDE), guide);
    float3 normal = pt::shading_normal(referenceHit, reference);
    pt::DenoiseGuides expectedGuide =
        pt::denoise_surface(ModScene().energy(), reference, normal, geometry.wo, incident);
    probe_output[base + 14] = guide.value.normal_roughness;
    probe_output[base + 15] = float4(normal, reference.pbr.base_roughness.w);
    probe_output[base + 16] = float4(guide.value.diffuse, 0);
    probe_output[base + 17] = float4(expectedGuide.diffuse_albedo, 0);
    probe_output[base + 18] = float4(guide.value.specular, 0);
    probe_output[base + 19] = float4(expectedGuide.specular_albedo, 0);
    restir_pt::InitialShading legacy =
        scene.shadeInitial(actual, float3(0.2, 0.3, 0.4), geometry.wo, -geometry.wo, false);
    probe_output[base + 20] = float4(legacy.emission, 0);
    probe_output[base + 21] = float4(reference.emission, 0);
    float3 random = float3((float2(pixel) + 0.5) / float2(parameters[0].image.xy),
                           (float((pixel.x * 17u + pixel.y * 13u) & 31u) + 0.5) / 32.0);
    restir_pt::BsdfSample sampled = scene.sampleBsdf(actual, random);
    openpbr::Sample expected = openpbr::sample(ModScene().energy(), reference.pbr, normal,
                                               actual.wo, random, incident, reference.transmitted);
    probe_output[base + 22] = float4(sampled.direction, sampled.pdf);
    probe_output[base + 23] = float4(expected.direction, expected.pdf);
    probe_output[base + 24] =
        float4(sampled.weight, uint(sampled.delta) | (uint(sampled.transmission) << 1));
    probe_output[base + 25] =
        float4(expected.pdf > 0 ? expected.response / expected.pdf : float3(0),
               uint((expected.events & openpbr::DISCRETE) != 0) |
                   (uint((expected.events & openpbr::TRANSMISSION) != 0) << 1));
    probe_output[base + 26] = sampled.medium;
    probe_output[base + 27] = expected.medium;
    restir_pt::BsdfEvaluation evaluated = scene.evaluateBsdf(actual, expected.direction, -1);
    openpbr::Evaluation expectedEvaluation =
        openpbr::evaluate(ModScene().energy(), reference.pbr, normal, actual.wo, expected.direction,
                          incident, reference.transmitted);
    probe_output[base + 28] = float4(evaluated.value, evaluated.pdf);
    probe_output[base + 29] = float4(expectedEvaluation.response, expectedEvaluation.pdf);
    probe_output[base + 30] = float4(1, material_id(primitive), 0, 0);
}
