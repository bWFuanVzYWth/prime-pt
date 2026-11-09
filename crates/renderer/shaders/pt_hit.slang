/// 普通PT生产入口；材质来源单独适配，路径和数值叶子由引擎维护。
module pt_hit;
#ifndef PT_HIT_SER
#define PT_HIT_SER 0
#endif
#ifndef PRIMEPT_RECONSTRUCTION
#define PRIMEPT_RECONSTRUCTION 1
#endif
import pt.hit_transport;
import pt.material_vertex;
import pt.scene;
import pt.denoise;
import pt.integrator;
import pt.path_sampler;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;
import params;
import geometry.camera;
import mod_scene;
import mod_decode;
#if PRIMEPT_RECONSTRUCTION
import mod_guides;
#endif
import primary_cache;
#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from a material key or an explicit raygen placeholder
#endif
#ifndef OPENPBR_VARIANT
#error OPENPBR_VARIANT must come from a material key or an explicit raygen placeholder
#endif
#ifndef PRIMEPT_LIGHT_MASK
#define PRIMEPT_LIGHT_MASK 3
#endif
#ifndef PRIMEPT_RECONSTRUCTION
#define PRIMEPT_RECONSTRUCTION 1
#endif
[[vk::binding(7, 0)]]
RWStructuredBuffer<float4> output_image;
[[vk::binding(8, 0)]]
RWStructuredBuffer<float4> accumulation;
[[vk::binding(9, 0)]]
RWStructuredBuffer<uint> sample_counts;
[[vk::binding(15, 0)]]
RWStructuredBuffer<float4> prefix_image;
public struct HitScene : pt::IHitScene
{
    public bool guides_enabled()
    {
        return PRIMEPT_RECONSTRUCTION != 0 && parameters[0].reconstruction.w == 2;
    }
    public uint program(HitObject object, float4 incident)
    {
        uint primitive = region_primitive(object.GetInstanceID(), object.GetPrimitiveIndex());
        return material_words.Load(material_id(primitive) * 112u + 64u) >> 2u;
    }
    public pt::SurfaceMaterial resolve(pt::Hit hit, uint cap, float4 incident)
    {
        return ModScene().resolve(hit, cap, incident);
    }
    public pt::SurfaceMaterial resolve_flow(pt::Hit hit, uint flow, float4 incident)
    {
        return ModScene().resolve_flow(hit, flow, incident);
    }
    public openpbr::Energy energy()
    {
        return ModScene().energy();
    }
    public pt::LightSample sample_sun(geometry::SurfacePoint p, float2 u)
    {
        return ModScene().sample_sun(p, u);
    }
    public pt::LightSample sample_light(geometry::SurfacePoint p, float3 u)
    {
        return ModScene().sample_light(p, u);
    }
    public float3 light_emission(geometry::SurfacePoint p, pt::LightSample s, bool solar)
    {
        return ModScene().light_emission(p, s, solar);
    }
    public float emission_pdf(pt::Hit hit, float3 previous)
    {
        return ModScene().emission_pdf(hit, previous);
    }
    public bool occluded(geometry::Ray ray)
    {
        return ModScene().occluded(ray);
    }
    public pt::Environment environment(float3 d, float3 p, uint depth)
    {
        return ModScene().environment(d, p, depth);
    }
    public RaytracingAccelerationStructure acceleration()
    {
        return scene;
    }
};
[shader("closesthit")]
void shade_hit(inout pt::MaterialCall call, BuiltInTriangleIntersectionAttributes attributes)
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
    pt::SurfaceMaterial material = decode_surface<AtlasColor, CachedAux>(
        hit, parameters[0].block_emission_scale, { auxiliary });
    pt::shade_material<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK>(
        ModScene(), call, hit, material, WorldRayDirection(), settings(parameters[0]));
}
// 只执行相机射线和覆盖测试，不解码材质或生成光路状态。
[shader("raygeneration")]
void primary_main()
{
    Params p = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = primary_pixel();
    pt::PathSampler sampler = pt::path_sampler(coordinate, p.rr_sample, p.options.z, p.options.x);
    float2 offset = p.reconstruction.w > 0 ? 0.5 + p.reconstruction.xy : sampler.film();
    float2 film = (float2(coordinate) + offset) / float2(p.image.xy);
    float3 direction =
        geometry::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
    RayDesc ray = { p.origin.xyz, 0, direction, pt::FAR };
    uint unused = 0;
    HitObject found = HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
    uint4 identity = uint4(0xffffffffu, 0, 0, 0);
    float distance = pt::FAR;
    if (found.IsHit())
    {
        BuiltInTriangleIntersectionAttributes attributes =
            found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        identity = uint4(region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex()),
                         asuint(attributes.barycentrics), 0);
        distance = found.GetRayDesc().TMax;
    }
    primary_identity[pixel] = identity;
    primary_ray[pixel] = float4(direction, distance);
}

[shader("raygeneration")]
void prefix_empty()
{
}
#ifndef PT_TRANSPORT_LANE
#define PT_TRANSPORT_LANE -1
#endif

[shader("raygeneration")]
void render_fused()
{

    Params params = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = coordinate.y * params.image.x + coordinate.x;
    bool diagnostic = (params.output_size.z & 1u) != 0;
    if (!diagnostic && accumulation[pixel].w < 0)
    {
        output_image[pixel] = float4(0, 0, 0, accumulation[pixel].w);
        return;
    }
    const bool separated = PT_TRANSPORT_LANE >= 0;
    bool realtime = separated || realtime_pt();
#if PT_TRANSPORT_LANE >= 0
    const uint first_lane = PT_TRANSPORT_LANE;
    const uint last_lane = PT_TRANSPORT_LANE + 1;
    const uint paths = PT_TRANSPORT_LANE == 0 ? 1 : 2;
#else
    uint paths = realtime ? params.image.w : 1u;
    uint first_lane = 0;
    uint last_lane = paths;
#endif
    pt::TransportCall data = {};
#if PT_TRANSPORT_LANE == 1
    // 与单pass相同的加法顺序；第一条路径的失败状态不再启动第二条。
    float4 previous = output_image[pixel];
    if (previous.w < 0)
    {
        output_image[pixel] = float4(previous.xyz * 0.5, previous.w);
        return;
    }
    data.path.radiance = previous.xyz;
#endif
    float distance = pt::FAR;
    for (uint lane = first_lane; lane < last_lane; lane++)
    {
        data.path.status = pt::PATH_ACTIVE;
        data.path.depth = 0;
        data.path.dielectric_events = 0;
        data.path.previous = 0;
        data.path.previous_pdf = 0;
        data.pixel = coordinate;
        data.sample = params.rr_sample + (realtime ? lane
                                                   : (sample_counts[pixel] >= params.image.z
                                                          ? sample_counts[pixel] - params.image.z
                                                          : 0u));
        float2 film = 0;
        data.path.origin = params.origin.xyz;
        data.path.throughput = 1;
        data.path.eta_scale = 1;
        data.path.medium = params.initial_medium;
        if (realtime)
        {
            data.path.direction = primary_ray[pixel].xyz;
            uint primitive = primary_identity[pixel].x;
            if (primitive == 0xffffffffu)
                pt::escape(HitScene(), data.path);
            else
            {
                uint program = material_words.Load(material_id(primitive) * 112u + 64u) >> 2u;
                pt::MaterialCall call =
                    pt::MaterialCall::primaryInput(data.sample, lane, data.path.radiance);
#if PT_HIT_SER
                ReorderThread(program, 32u);
#endif
                CallShader(program, call);
                data.path = call.result();
            }
        }
        else
        {
            pt::PathSampler sampler =
                pt::path_sampler(coordinate, data.sample, params.options.z, params.options.x);
            film =
                (float2(coordinate) +
                 (params.reconstruction.w > 0 ? 0.5 + params.reconstruction.xy : sampler.film())) /
                float2(params.image.xy);
            data.path.direction = geometry::camera_scaled_direction(
                params.forward.xyz, params.right.xyz, params.up.xyz, film);
        }
#if PRIMEPT_RECONSTRUCTION
        pt::DenoiseCamera current = geometry::camera_from_scaled(
            params.origin.xyz, params.forward.xyz, params.right.xyz, params.up.xyz);
        pt::DenoiseCamera previous = { params.previous_position, params.previous_forward,
                                       params.previous_right, params.previous_up };
        Guides guides = { coordinate, film, current, previous };
        if (params.reconstruction.w == 2)
            distance = pt::trace_hit_path(HitScene(), guides, data);
        else
            distance = pt::trace_hit_path(HitScene(), pt::NoPrefixGuides(), data);
#else
        distance = pt::trace_hit_path(HitScene(), pt::NoPrefixGuides(), data);
#endif
        if (data.path.status == pt::PATH_INVALID || data.path.status == pt::PATH_DIELECTRIC_LIMIT)
            break;
    }
#if PT_TRANSPORT_LANE != 1
    if (realtime)
        distance = primary_ray[pixel].w;
    prefix_image[pixel] = float4(0, 0, 0, distance);
#endif
    output_image[pixel] = float4(data.path.radiance / float(paths),
                                 data.path.status == pt::PATH_INVALID            ? -1
                                 : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                 : 0);
}
