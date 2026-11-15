/// 普通PT生产入口；材质来源单独适配，路径和数值叶子由引擎维护。
module pt_hit;
#include "geometry/opacity.slang"
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
#ifdef PRIMEPT_SER
static const let HitReorder = true;
#else
static const let HitReorder = false;
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
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_hit)
#endif
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
func cache_primary<let FrameFilm : bool>()->void
{
    Params p = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = primary_pixel();
    pt::PathSampler sampler = pt::path_sampler(coordinate, p.rr_sample, p.options.z, p.options.x);
    float2 offset = FrameFilm ? 0.5 + p.reconstruction.xy : sampler.film();
    let film = geometry::camera_sample_uv_offset(coordinate, p.inverse_extent, offset);
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
void prefix_empty() {
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
    let previous : pt::DenoiseCamera = { p.previous_position, p.previous_forward, p.previous_right,
                                         p.previous_up };
    let guides : Guides = { coordinate, film, current, previous };
    if (p.reconstruction.w == 2)
        return pt::trace_hit_path<HitScene, Guides, HitReorder>(HitScene(), guides, data);
#endif
    return pt::trace_hit_path<HitScene, pt::NoPrefixGuides, HitReorder>(HitScene(),
                                                                        pt::NoPrefixGuides(), data);
}

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
        data.path.direction = primary_ray[pixel].xyz;
        let primitive = primary_identity[pixel].x;
        if (primitive == 0xffffffffu)
            pt::escape(HitScene(), data.path);
        else
        {
            let program = material_words.Load(material_id(primitive) * 112u + 64u) >> 2u;
            var call = pt::MaterialCall::primaryInput(data.sample, lane, data.path.radiance);
            if (HitReorder)
                ReorderThread(program, 32u);
            CallShader(program, call);
            data.path = call.result();
        }
        return 0;
    }
    public func trace(inout data: pt::TransportCall, film: float2)->float
    {
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
        let film = geometry::camera_sample_uv_offset(coordinate, p.inverse_extent, offset);
        data.path.direction =
            geometry::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
        return film;
    }
    public func trace(inout data: pt::TransportCall, film: float2)->float
    {
        return trace_path(coordinate, film, data);
    }
};

func render_pixel<A : pt::IPixelTransport, let Realtime : bool, let Samples : uint, let Lane : int,
                  let IgnoreFailure : bool>(adapter: A, pixel: uint, coordinate: uint2)
    ->void
{
    if (!IgnoreFailure && accumulation[pixel].w < 0)
    {
        output_image[pixel] = float4(0, 0, 0, accumulation[pixel].w);
        return;
    }
    let input : pt::PixelTransportInput = { coordinate };
    var previous = float4(0);
    if (Lane == 1)
        previous = output_image[pixel];
    let result = pt::integrate_pixel<A, Realtime, Samples, Lane>(adapter, input, previous);
    if (Lane != 1)
        prefix_image[pixel] = float4(0, 0, 0, Realtime ? primary_ray[pixel].w : result.distance);
    output_image[pixel] = result.radiance;
}

[shader("raygeneration")]
func render_rt1()->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(render_rt1)
#endif
    let coordinate = DispatchRaysIndex().xy;
    let pixel = primary_pixel();
    let adapter : RealtimePixelAdapter = { coordinate, pixel };
    render_pixel<RealtimePixelAdapter, true, 1, -1, false>(adapter, pixel, coordinate);
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
    render_pixel<RealtimePixelAdapter, true, 2, 0, false>(adapter, pixel, coordinate);
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
    render_pixel<RealtimePixelAdapter, true, 2, 1, false>(adapter, pixel, coordinate);
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
    render_pixel<OfflinePixelAdapter, false, 1, -1, false>(adapter, pixel, coordinate);
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
    render_pixel<RealtimePixelAdapter, true, 2, -1, false>(adapter, pixel, coordinate);
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
        render_pixel<OfflinePixelAdapter, false, 1, -1, true>(adapter, pixel, coordinate);
    }
    else if (realtime_pt())
    {
        let adapter : RealtimePixelAdapter = { coordinate, pixel };
        if (parameters[0].image.w == 2)
            render_pixel<RealtimePixelAdapter, true, 2, -1, false>(adapter, pixel, coordinate);
        else
            render_pixel<RealtimePixelAdapter, true, 1, -1, false>(adapter, pixel, coordinate);
    }
    else
    {
        let adapter : OfflinePixelAdapter = { coordinate, pixel };
        render_pixel<OfflinePixelAdapter, false, 1, -1, false>(adapter, pixel, coordinate);
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
    render_pixel<OfflinePixelAdapter, false, 1, -1, true>(adapter, pixel, coordinate);
}
