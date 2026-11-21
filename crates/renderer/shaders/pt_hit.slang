/// 普通PT生产入口；材质来源单独适配，路径和数值叶子由引擎维护。
module pt_hit;
import nrd_path;
import nrd_guides;
import pt.reconstruction;
import known_geometry;
#include "opacity/micromap.inc.slang"
#ifndef PRIMEPT_OMM
#define PRIMEPT_OMM 0
#endif
#ifndef PRIMEPT_RECONSTRUCTION
#define PRIMEPT_RECONSTRUCTION 1
#endif
#ifndef PRIMEPT_REFERENCE_FILM
#define PRIMEPT_REFERENCE_FILM 0
#endif
import pt.hit_transport;
import pt.pixel_transport;
import pt.material_vertex;
import pt.scene;
import pt.integrator;
import pt.path_sampler;
import geometry.ray;
import openpbr.api;
import openpbr.flow;
import params;
import camera.pinhole;
import mod_scene;
import mod_decode;
#if PRIMEPT_RECONSTRUCTION
import mod_guides;
#endif
import primary_cache;
import light_sampling.sample;
import transport.surface_material;
#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from a material key or an explicit raygen placeholder
#endif
#ifndef OPENPBR_VARIANT
#error OPENPBR_VARIANT must come from a material key or an explicit raygen placeholder
#endif
#ifndef PRIMEPT_LIGHT_MASK
#define PRIMEPT_LIGHT_MASK 3
#endif
#ifdef PRIMEPT_SER
static const let hit_reorder = true;
#else
static const let hit_reorder = false;
#endif
#ifndef PRIMEPT_RECONSTRUCTION
#define PRIMEPT_RECONSTRUCTION 1
#endif
[[vk::binding(7, 0)]]
var output_image : RWStructuredBuffer<float4>;
[[vk::binding(8, 0)]]
var accumulation : RWStructuredBuffer<float4>;
[[vk::binding(9, 0)]]
var sample_counts : RWStructuredBuffer<uint>;
public struct HitScene : pt::IHitScene
{
    public override func path_mask(primary: bool)->uint
    {
        return path_visibility_mask(primary);
    }
    public func guides_enabled()->bool
    {
        return PRIMEPT_RECONSTRUCTION != 0 && parameters[0].reconstruction.w == 2;
    }
    public func program(object: HitObject, incident: float4)->uint
    {
        let primitive : uint = region_primitive(object.GetInstanceID(), object.GetPrimitiveIndex());
        return material_words.Load(material_id(primitive) * 112u + 64u) >> 2u;
    }
    public func resolve(hit: pt::Hit, cap: uint, incident: float4)->transport::SurfaceMaterial
    {
        return ModScene().resolve(hit, cap, incident);
    }
    public func resolve_flow(hit: pt::Hit, flow: uint, incident: float4)->transport::SurfaceMaterial
    {
        return ModScene().resolve_flow(hit, flow, incident);
    }
    public func energy()->openpbr::Energy
    {
        return ModScene().energy();
    }
    public func sample_sun(p: geometry::SurfacePoint, u: float2)->light_sampling::LightSample
    {
        return ModScene().sample_sun(p, u);
    }
    public func sample_light(p: geometry::SurfacePoint, u: float3)->light_sampling::LightSample
    {
        return ModScene().sample_light(p, u);
    }
    public func light_emission(p: geometry::SurfacePoint, s: light_sampling::LightSample,
                               solar: bool)
        ->float3
    {
        return ModScene().light_emission(p, s, solar);
    }
    public func emission_pdf(hit: pt::Hit, previous: float3)->float
    {
        return ModScene().emission_pdf(hit, previous);
    }
    public func occluded(ray: geometry::Ray)->bool
    {
        return ModScene().occluded(ray);
    }
    public func environment(d: float3, p: float3, depth: uint)->pt::Environment
    {
        return ModScene().environment(d, p, depth);
    }
    public func acceleration()->RaytracingAccelerationStructure
    {
        return scene;
    }
};
[shader("closesthit")]
func shade_hit(inout call: pt::MaterialCall, attributes: BuiltInTriangleIntersectionAttributes)
    ->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_hit)
#endif
    let a : float3 = HitTriangleVertexPosition(0);
    let b : float3 = HitTriangleVertexPosition(1);
    let c : float3 = HitTriangleVertexPosition(2);
    var hit : pt::Hit = {};
    hit.surface = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                           WorldToObject3x4());
    let primitive : uint = region_primitive(InstanceID(), PrimitiveIndex());
    let auxiliary = material_aux<OPENPBR_VARIANT>(primitive, attributes.barycentrics);
    var normal : uint = 0;
    if ((OPENPBR_VARIANT & 8u) != 0)
        normal = map_normal_xy(primitive, auxiliary.normal_ndf.xy, b - a, c - a,
                               (float3x3)ObjectToWorld3x4(), hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = RayTCurrent();
    hit.front = dot(WorldRayDirection(), hit.surface.normal) < 0;
    let material : transport::SurfaceMaterial = decode_surface<AtlasColor, CachedAux>(
                       hit, parameters[0].block_emission_scale, { auxiliary });
    pt::shade_material<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK>(
        ModScene(), call, hit, material, WorldRayDirection(), settings(parameters[0]));
}
// 只执行相机射线和覆盖测试，不解码材质或生成光路状态。
func cache_primary<let frame_film : bool>()->void
{
    let p : Params = parameters[0];
    let coordinate : uint2 = DispatchRaysIndex().xy;
    let pixel : uint = primary_pixel();
    let direction = primary_direction<frame_film>(p, coordinate);
    let ray : RayDesc = { p.origin.xyz, 0, direction, pt::FAR };
    var unused : uint = 0;
    var found : HitObject = HitObject::TraceRay(scene, RAY_FLAG_NONE, path_visibility_mask(true), 0,
                                                1, 0, ray, unused);
    var identity = uint4(0xffffffffu, 0, 0, 0);
    var distance : float = pt::FAR;
    if (found.IsHit())
    {
        let attributes : BuiltInTriangleIntersectionAttributes =
                             found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        identity = uint4(region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex()),
                         asuint(attributes.barycentrics), 0);
        distance = found.GetRayDesc().TMax;
    }
    primary_hit[pixel] = uint4(identity.xyz, asuint(distance));
}

[shader("raygeneration")]
func primary_random()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(primary_random)
#endif
    cache_primary<false>();
}

[shader("raygeneration")]
func primary_jittered()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(primary_jittered)
#endif
    cache_primary<true>();
}

// 测试和发行夹具保留可显式选择的原film参考入口。
[shader("raygeneration")]
func primary_main()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(primary_main)
#endif
    if (parameters[0].reconstruction.w > 0)
        cache_primary<true>();
    else
        cache_primary<false>();
}

[shader("raygeneration")]
func prefix_empty()
    ->void {
#if PRIMEPT_OMM
        ENGINE_OPACITY_MICROMAP(prefix_empty)
#endif
    }

func trace_path(coordinate: uint2, film: float2, inout data: pt::TransportCall)
    ->float
{
#if PRIMEPT_RECONSTRUCTION
    let p = parameters[0];
    let current = p.current_camera;
    let previous : camera::Camera = { p.previous_position, p.previous_forward, p.previous_right,
                                      p.previous_up };
    let guides : Guides = { coordinate, film, current, previous };
    if (p.reconstruction.w == 2)
        return pt::trace_hit_path<HitScene, Guides, hit_reorder>(HitScene(), guides, data);
#endif
    return pt::trace_hit_path<HitScene, pt::NoPrefixGuides, hit_reorder>(
        HitScene(), pt::NoPrefixGuides(), data);
}

func known_hit(identity: uint4, direction: float3, distance: float)->pt::Hit
{
    var hit : pt::Hit = {};
    let bary = asfloat(identity.yz);
    hit.surface = geometry::surface_point(known_endpoint(identity.x, bary));
    hit.identity = identity;
    if (has_authored_normal(identity.x))
        hit.identity.w = known_normal<~0u>(identity.x, bary, hit.surface.normal);
    hit.front = dot(direction, hit.surface.normal) < 0;
    hit.distance = distance;
    return hit;
}

struct HitGuideScene : pt::IGuideScene
{
    func intersect(origin: float3, direction: float3, out hit: pt::Hit)->bool
    {
        let ray : RayDesc = { origin, 0, direction, pt::FAR };
        var unused = 0u;
        var found = HitObject::TraceRay(scene, RAY_FLAG_NONE, visibility_mask(ray.Origin), 0, 1, 0,
                                        ray, unused);
        hit = {};
        if (!found.IsHit())
            return false;
        let attr = found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        let primitive = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
        hit = known_hit(uint4(primitive, asuint(attr.barycentrics), 0), direction,
                        found.GetRayDesc().TMax);
        return true;
    }
    func resolve_hit(inout hit: pt::Hit, incident: float4)->void
    {
    }
    func guide_material(hit: pt::Hit, incident: float4)->transport::SurfaceMaterial
    {
        return ModScene().guide_material(hit, incident);
    }
    func energy()->openpbr::Energy
    {
        return ModScene().energy();
    }
    func guide_pose(hit: pt::Hit)->pt::GuidePose
    {
        return ModScene().guide_pose(hit);
    }
};

public struct RealtimePixelAdapter : pt::IPixelTransport
{
    public var coordinate : uint2;
    public var pixel : uint;
    public func sample_index(lane: uint)->uint
    {
        return parameters[0].rr_sample + lane;
    }
    public func origin()->float3
    {
        return parameters[0].origin.xyz;
    }
    public func initial_medium()->float4
    {
        return parameters[0].initial_medium;
    }
    public func start_path(inout data: pt::TransportCall, lane: uint)->float2
    {
        data.path.direction = primary_direction(parameters[0], coordinate);
        let primitive = primary_hit[pixel].x;
        if (primitive == 0xffffffffu)
        {
            if (lane == 0 && parameters[0].reconstruction.w == 3)
                sky_signal(coordinate);
            pt::escape(HitScene(), data.path);
        }
        else
        {
            let start = material_id(primitive) * 112u + 64u;
            if (lane == 0 && parameters[0].reconstruction.w == 3 &&
                fixed_guide_candidate(material_words.Load(start + 8u) >> 20))
            {
                data.hit = known_hit(uint4(primary_hit[pixel].xyz, 0), data.path.direction,
                                     asfloat(primary_hit[pixel].w));
                write_fixed_guides(HitGuideScene(), data);
            }
            let program = material_words.Load(start) >> 2u;
            var call = pt::MaterialCall::primary_input(data.sample, lane, data.path.radiance);
            if (hit_reorder)
                ReorderThread(program, 32u);
            CallShader(program, call);
            data.path = call.result();
        }
        return 0;
    }
    public func trace(inout data: pt::TransportCall, film: float2)->float
    {
        if (parameters[0].reconstruction.w == 3 && primary_hit[pixel].x != 0xffffffffu)
        {
            let before = data.path.radiance;
            data.path.radiance = 0;
            let direction = data.path.direction;
            let distance = pt::trace_hit_path<HitScene, pt::NoPrefixGuides, hit_reorder, true>(
                HitScene(), pt::NoPrefixGuides(), data);
            finish_signal(pixel * 2 + (data.sample - parameters[0].rr_sample),
                          max(data.path.radiance, 0), direction, distance);
            data.path.radiance += before;
            return distance;
        }
        return trace_path(coordinate, film, data);
    }
};

public struct OfflinePixelAdapter : pt::IPixelTransport
{
    public var coordinate : uint2;
    public var pixel : uint;
    public func sample_index(lane: uint)->uint
    {
        let p = parameters[0];
        return pt::pixel_sample_index(p.rr_sample, lane, false, sample_counts[pixel], p.image.z);
    }
    public func origin()->float3
    {
        return parameters[0].origin.xyz;
    }
    public func initial_medium()->float4
    {
        return parameters[0].initial_medium;
    }
    public func start_path(inout data: pt::TransportCall, lane: uint)->float2
    {
        let p = parameters[0];
        let sampler = pt::path_sampler(coordinate, data.sample, p.options.z, p.options.x);
        var offset = sampler.film();
#if PRIMEPT_REFERENCE_FILM
        // 居中film仅供独立正确性/质量对照；生产离线不启用RR。
        if (p.reconstruction.w > 0)
            offset = 0.5 + p.reconstruction.xy;
#endif
        let film = camera::camera_sample_uv_offset(coordinate, p.inverse_extent, offset);
        data.path.direction =
            camera::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
        return film;
    }
    public func trace(inout data: pt::TransportCall, film: float2)->float
    {
        return trace_path(coordinate, film, data);
    }
};

func render_pixel<A : pt::IPixelTransport, let realtime : bool, let lane : int,
                  let ignore_failure : bool>(adapter: A, pixel: uint, coordinate: uint2)
    ->void
{
    if (!ignore_failure && (!realtime || (parameters[0].output_size.z & 64u) == 0) &&
        accumulation[pixel].w < 0)
    {
        output_image[pixel] = float4(0, 0, 0, accumulation[pixel].w);
        return;
    }
    var previous = float4(0);
    if (lane == 1)
        previous = output_image[pixel];
    let result = pt::integrate_pixel<A, realtime, lane>(adapter, coordinate, previous);
    if (lane != 1 && (!realtime || parameters[0].reconstruction.w != 2))
    {
        let distance = realtime ? asfloat(primary_hit[pixel].w) : result.distance;
        if (realtime && parameters[0].reconstruction.w == 3 && distance != pt::FAR)
            prefix_image[pixel].w = distance;
        else
            prefix_image[pixel] = float4(0, 0, 0, distance);
    }
    output_image[pixel] = result.radiance;
}

[shader("raygeneration")]
func render_rt2_lane0()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_rt2_lane0)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    let adapter : RealtimePixelAdapter = { coordinate, pixel };
    render_pixel<RealtimePixelAdapter, true, 0, false>(adapter, pixel, coordinate);
}

[shader("raygeneration")]
func render_rt2_lane1()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_rt2_lane1)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    let adapter : RealtimePixelAdapter = { coordinate, pixel };
    render_pixel<RealtimePixelAdapter, true, 1, false>(adapter, pixel, coordinate);
}

[shader("raygeneration")]
func render_offline()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_offline)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    let adapter : OfflinePixelAdapter = { coordinate, pixel };
    render_pixel<OfflinePixelAdapter, false, -1, false>(adapter, pixel, coordinate);
}

[shader("raygeneration")]
func render_rt2_merged()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_rt2_merged)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    let adapter : RealtimePixelAdapter = { coordinate, pixel };
    render_pixel<RealtimePixelAdapter, true, -1, false>(adapter, pixel, coordinate);
}

// 原混合入口只供显式参考/诊断；生产链接到上面的固定执行计划。
[shader("raygeneration")]
func render_fused()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_fused)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    if ((parameters[0].output_size.z & 1u) != 0)
    {
        let adapter : OfflinePixelAdapter = { coordinate, pixel };
        render_pixel<OfflinePixelAdapter, false, -1, true>(adapter, pixel, coordinate);
    }
    else if (realtime_pt())
    {
        let adapter : RealtimePixelAdapter = { coordinate, pixel };
        render_pixel<RealtimePixelAdapter, true, -1, false>(adapter, pixel, coordinate);
    }
    else
    {
        let adapter : OfflinePixelAdapter = { coordinate, pixel };
        render_pixel<OfflinePixelAdapter, false, -1, false>(adapter, pixel, coordinate);
    }
}

// 完整PT诊断参考仍着色真实材质；仅测试配置准备，不链接到生产计划。
[shader("raygeneration")]
func render_diagnostic()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_diagnostic)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    let adapter : OfflinePixelAdapter = { coordinate, pixel };
    render_pixel<OfflinePixelAdapter, false, -1, true>(adapter, pixel, coordinate);
}
