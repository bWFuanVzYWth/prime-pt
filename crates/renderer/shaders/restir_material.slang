/// 资源准备时按真实Flow、材质解码能力和操作编译；材料相关控制域留在专用叶子。
module restir_material;
import mod_scene;
import restir_scene;
import restir_call;
import restir_geometry;
import restir_pt.initial_vertex;
import restir_pt.material;
import restir_pt.storage;
import restir_pt.scene;
import restir_pt.types;
import pt.scene;
import pt.transport;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;

#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from resource preparation
#endif
#ifndef PRIMEPT_LIGHT_MASK
#define PRIMEPT_LIGHT_MASK 3
#endif
// 普通PT按太阳/局部灯排列；初采按局部灯/太阳排列。
static const uint INITIAL_LIGHT_MASK =
    ((PRIMEPT_LIGHT_MASK & 2u) >> 1u) | ((PRIMEPT_LIGHT_MASK & 1u) << 1u);
[[vk::binding(11, 0)]]
RWByteAddressBuffer initial_cv_proxy;

pt::Hit known_material_hit(RestirMaterialHit source)
{
    pt::Hit hit = to_pt_hit(source);
    hit.identity.w = restir_normal<OPENPBR_VARIANT>(hit.identity.x, asfloat(hit.identity.yz),
                                                    hit.surface.normal);
    return hit;
}

pt::Hit initial_material_hit(inout restir_pt::Surface surface
#ifdef PRIMEPT_INITIAL_HIT
                             ,
                             BuiltInTriangleIntersectionAttributes attributes
#endif
)
{
    pt::Hit hit;
#if defined(PRIMEPT_INITIAL_HIT) && (defined(PRIMEPT_HIT_GEOMETRY) || defined(PRIMEPT_HIT_GEOMETRY_LEAF))
    float3 a = HitTriangleVertexPosition(0), b = HitTriangleVertexPosition(1),
           c = HitTriangleVertexPosition(2);
    surface.point = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                             WorldToObject3x4());
    uint primitive = region_primitive(InstanceID(), PrimitiveIndex());
    uint normal = 0;
    if ((OPENPBR_VARIANT & 8u) != 0)
        normal = mapped_shading_normal_known(primitive, attributes.barycentrics, b - a, c - a,
                                             (float3x3)ObjectToWorld3x4(), surface.point.normal);
    surface.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    surface.wo = -WorldRayDirection();
    surface.frontFacing = dot(surface.wo, surface.point.normal) >= 0;
    hit = restir_hit(surface);
#else
    hit = known_material_hit(material_hit(surface));
#endif
    hit.surface = surface.point;
    return hit;
}

restir_pt::InitialVertex shade_initial_surface(restir_pt::Surface surface, pt::Hit hit,
                                               restir_pt::InitialRequest request)
{
    ModScene provider;
    bool proxy = (request.flags & restir_pt::INITIAL_PRIMARY) != 0 &&
                 (parameters[0].output_size.z & 8u) != 0;
    if ((OPENPBR_FLOW & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                         openpbr::FLOW_CONDUCTOR_REFLECTION | openpbr::FLOW_EMISSION)) != 0 ||
        proxy)
        hit.texel = material_texel(hit.identity.x, asfloat(hit.identity.yz));
    if (proxy)
    {
        uint2 pixel = DispatchRaysIndex().xy;
        restir_pt::store_color(initial_cv_proxy, pixel.y * parameters[0].image.x + pixel.x,
                               max(hit.texel.xyz, 0.04));
    }
    pt::SurfaceMaterial material =
        resolve_material(hit, OPENPBR_FLOW, OPENPBR_VARIANT, parameters[0].block_emission_scale);
    surface.transmitted = material.transmitted;
    restir_pt::InitialVertex result =
        restir_pt::shade_initial_vertex<OPENPBR_FLOW, RestirScene, INITIAL_LIGHT_MASK>(
            RestirScene(), surface, material, provider.energy(), request);
    if ((OPENPBR_FLOW & openpbr::FLOW_EMISSION) != 0 &&
        (request.flags & restir_pt::INITIAL_PRIMARY) == 0 && any(result.emission > 0))
        result.emissionPdf = provider.emission_pdf(hit, request.previousPosition);
    return result;
}

#ifdef PRIMEPT_INITIAL_HIT
[shader("closesthit")]
#else
[shader("callable")]
#endif
void restir_initial_flow(inout RestirInitialCall call
#ifdef PRIMEPT_INITIAL_HIT
                         ,
                         BuiltInTriangleIntersectionAttributes attributes
#endif
)
{
    restir_pt::Surface surface = call.surface();
    pt::Hit hit = initial_material_hit(surface
#ifdef PRIMEPT_INITIAL_HIT
                                       ,
                                       attributes
#endif
    );
    restir_pt::InitialRequest request = call.request();
    call.store(shade_initial_surface(surface, hit, request));
}

#ifdef PRIMEPT_INITIAL_HIT
[shader("closesthit")]
#else
[shader("callable")]
#endif
void restir_initial_direct(inout RestirInitialDirectCall call
#ifdef PRIMEPT_INITIAL_HIT
                           ,
                           BuiltInTriangleIntersectionAttributes attributes
#endif
)
{
    restir_pt::Surface surface = call.surface();
    pt::Hit hit = initial_material_hit(surface
#ifdef PRIMEPT_INITIAL_HIT
                                       ,
                                       attributes
#endif
    );
    restir_pt::InitialRequest request = call.request();
    request.flags &= ~restir_pt::INITIAL_SAMPLE;
    call.store(shade_initial_surface(surface, hit, request));
}

[shader("callable")]
void restir_replay_flow(inout RestirReplayCall call)
{
    pt::Hit hit = known_material_hit(call.hit);
    ModScene provider;
    pt::SurfaceMaterial material = provider.resolve_flow(hit, OPENPBR_FLOW, call.incident);
    openpbr::Sample sampled = openpbr::sample_flow<OPENPBR_FLOW>(
        provider.energy(), material.pbr, pt::shading_normal(hit, material), call.wo, call.u,
        call.incident, material.transmitted);
    restir_pt::Surface surface = {};
    surface.point = hit.surface;
    surface.wo = call.wo;
    surface.incident = call.incident;
    restir_pt::BsdfSample result =
        restir_pt::material_sample<OPENPBR_FLOW>(surface, material, sampled);
    call.hit.normal = result.direction;
    call.hit.identity = asuint(float4(result.pdf, result.weight));
    call.hit.flags = uint(result.delta) | (uint(result.transmission) << 1u);
    call.incident = result.medium;
}

[shader("callable")]
void restir_evaluate_flow(inout RestirEvaluateCall call)
{
    pt::Hit hit = known_material_hit(call.hit);
    ModScene provider;
    float4 incident = float4(call.incident_ior, 0, 0, 0);
    pt::SurfaceMaterial material = provider.resolve_flow(hit, OPENPBR_FLOW, incident);
    openpbr::Evaluation evaluated = openpbr::evaluate_flow<OPENPBR_FLOW>(
        provider.energy(), material.pbr, pt::shading_normal(hit, material), call.wo, call.direction,
        incident, material.transmitted);
    call.hit.identity = asuint(float4(evaluated.pdf, evaluated.response));
}

[shader("callable")]
void restir_connection_flow(inout RestirConnectionCall call)
{
    pt::Hit hit = known_material_hit(call.hit);
    ModScene provider;
    pt::SurfaceMaterial material = provider.resolve_flow(hit, OPENPBR_FLOW, call.incident);
    restir_pt::Surface surface = {};
    surface.point = hit.surface;
    surface.wo = call.wo;
    surface.incident = call.incident;
    restir_pt::ConnectionEvaluation result = restir_pt::evaluate_connection<OPENPBR_FLOW>(
        surface, material, provider.energy(), call.direction, call.distance);
    call.hit.normal = result.transmittance;
    call.hit.identity = asuint(float4(result.bsdf.pdf, result.bsdf.value));
}

[shader("callable")]
void restir_metadata_flow(inout RestirMetadataCall call)
{
    pt::Hit hit = {};
    hit.identity = asuint(call.first);
    hit.front = asuint(call.second.x) != 0;
    // 元数据只解码粗糙度/IOR/metalness；发光与逆向提案由各自叶子拥有。
    pt::SurfaceMaterial material =
        resolve_material(hit, OPENPBR_FLOW, OPENPBR_VARIANT, parameters[0].block_emission_scale);
    float4 incident = hit.front || (material.pbr.control.x & 3u) != 1u
                          ? float4(1, 0, 0, 0)
                          : float4(material.pbr.surface.x,
                                   material_at(material_id(hit.identity.x)).extinction.yzw);
    call.store(material, incident,
               !openpbr::is_discrete_only_flow<OPENPBR_FLOW>(material.pbr, incident,
                                                             material.transmitted));
}

[shader("callable")]
void restir_emitter_flow(inout RestirEmitterCall call)
{
    float3 emission = 0;
    float pdf = 0;
    float3 position = 0, normal = 0;
    if ((OPENPBR_FLOW & openpbr::FLOW_EMISSION) != 0)
    {
        restir_pt::HitInfo identity = call.identity();
        pt::Hit hit = {};
        hit.identity = uint4(restir_primitive(identity), identity.data.zw, 0);
        hit.surface =
            geometry::surface_point(restir_endpoint(hit.identity.x, asfloat(identity.data.zw)));
        hit.front = dot(call.normal, hit.surface.normal) >= 0;
        hit.texel = material_texel(hit.identity.x, asfloat(hit.identity.yz));
        emission =
            resolve_material(hit, OPENPBR_FLOW, OPENPBR_VARIANT, parameters[0].block_emission_scale)
                .emission;
        if (any(emission > 0))
            pdf = ModScene().emission_pdf(hit, call.position);
        position = hit.surface.position;
        normal = hit.surface.normal;
    }
    call.emission = emission;
    call.pdf = pdf;
    call.position = position;
    call.normal = normal;
}

[shader("callable")]
void restir_pdf_flow(inout RestirPdfCall call)
{
    restir_pt::HitInfo identity = { call.identity };
    pt::Hit hit = {};
    hit.identity = uint4(restir_primitive(identity), identity.data.zw, 0);
    hit.surface =
        geometry::surface_point(restir_endpoint(hit.identity.x, asfloat(identity.data.zw)));
    call.identity.x = asuint(ModScene().emission_pdf(hit, call.previous));
}
