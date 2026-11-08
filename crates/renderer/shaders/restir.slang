/// point ReSTIR PT：融合初采、双向时间shift、paired三邻居空间shift和最终合并。
module restir;
// Hit Group仅用于初采新命中；其他raygen不实例化或保存硬件命中上下文。
import initial_hit;
import restir_scene;
import restir_call;
import mod_scene;
import mod_guides;
import params;
import restir_pt.initial;
import restir_pt.types;
import restir_pt.rng;
import restir_pt.reservoir;
import restir_pt.shift;
import restir_pt.storage;
import restir_pt.admission;
import restir_pt.merge;
import restir_pt.resampling;
import restir_pt.schedule;
import restir_pt.paired;
import restir_pt.fixed_mixture;
import restir_pt.control_variate;
import geometry.camera;
import geometry.ray;
import pt.denoise;
import pt.integrator;

using namespace restir_pt;

[[vk::binding(7, 0)]]
RWStructuredBuffer<float4> restir_output;
[[vk::binding(15, 0)]]
RWStructuredBuffer<float4> restir_residual;
/// 与普通PT互斥使用landing normal plane；只消费其前12B/像素的紧凑RGB域。
/// 本帧initial写入、merge读取，不跨帧保存。
[[vk::binding(11, 0)]]
RWByteAddressBuffer cv_proxy;
[[vk::binding(38, 0)]]
RWByteAddressBuffer reservoirs_a;
[[vk::binding(39, 0)]]
RWByteAddressBuffer reservoirs_b;
[[vk::binding(40, 0)]]
RWStructuredBuffer<uint4> primary_current;
[[vk::binding(41, 0)]]
RWStructuredBuffer<uint4> primary_history;
[[vk::binding(42, 0)]]
RWStructuredBuffer<uint2> neighbor_masks;
[[vk::binding(43, 0)]]
RWByteAddressBuffer shifts;
[[vk::binding(44, 0)]]
StructuredBuffer<uint> paired_lut;
[[vk::binding(45, 0)]]
RWByteAddressBuffer colors_a;
[[vk::binding(46, 0)]]
RWByteAddressBuffer colors_b;

// 与game相同，系数只依赖固定primary的UV/tint纹理代理，不读取随机路径辐射。
float3 cv_reflectance(uint2 pixel)
{
    return load_color(cv_proxy, pixel.y * parameters[0].image.x + pixel.x);
}

uint offset_for(uint2 pixel)
{
    return reservoir_offset(pixel, screen_tiles(parameters[0].image.xy));
}
/// 当前帧的真实primary身份；离屏probe与复用阶段消费同一份输入。
public HitInfo primary(uint2 pixel)
{
    HitInfo hit;
    hit.data = primary_current[offset_for(pixel)];
    return hit;
}

PairedNeighborTransform transform_for_frame()
{
    Params p = parameters[0];
    TinyUniformSampleGenerator generator =
        rng_for_pixel(uint2(0), p.temporal_reserved.x ^ p.options.x);
    return paired_transform(generator.next(), p.image.xy);
}

Surface current_surface(RestirScene scene, uint2 pixel)
{
    Surface result = scene.loadSurfaceFromOrigin(primary(pixel), scene.initialOrigin(pixel), false);
    result.wo = -scene.initialDirection(pixel);
    scene.setIncident(result, scene.initialMedium(pixel));
    return result;
}

PrecomputedShiftData shifted(RestirScene scene, Surface destination, RWByteAddressBuffer source,
                             uint index, uint2 pixel, bool previous)
{
    ShiftSettings settings = {};
    return shiftPathLoaded(scene, destination, source, index,
                           scene.primaryRayFootprint(destination, pixel, previous), settings, true,
                           true, true, previous);
}

[shader("raygeneration")]
void restir_initial()
{
    uint2 pixel = DispatchRaysIndex().xy;
    Params p = parameters[0];
    uint linear = pixel.y * p.image.x + pixel.x;
    RestirScene scene = {};
    HitInfo hit;
    float distance;
#ifdef PRIMEPT_INITIAL_HIT
    InitialHitTrace tracer;
    HitObject traced;
    tracer.trace(scene, traced, scene.initialOrigin(pixel), scene.initialDirection(pixel), hit,
                 distance);
#else
    ModInitialTrace tracer;
    uint traced = 0;
    tracer.trace(scene, traced, scene.initialOrigin(pixel), scene.initialDirection(pixel), hit,
                 distance);
#endif
    primary_current[offset_for(pixel)] = hit.data;
    if ((p.output_size.z & 8u) != 0 && !hit.isValid())
        store_color(cv_proxy, linear, float3(0.04));
    TinyUniformSampleGenerator generator =
        rng_for_pixel(pixel, p.temporal_reserved.x ^ p.options.x);
    ShiftSettings settings = {};
#ifdef PRIMEPT_INITIAL_HIT
    InitialResult initial = sampleInitialPathTraced<RestirScene, HitObject, InitialHitTrace, 2>(
        scene, tracer, traced, pixel, hit, generator.state, p.options.y, settings, true, true,
        true);
#else
    InitialResult initial = sampleInitialPathTraced<RestirScene, uint, ModInitialTrace, 2>(
        scene, tracer, traced, pixel, hit, generator.state, p.options.y, settings, true, true,
        true);
#endif
    store_reservoir(reservoirs_a, offset_for(pixel), initial.reservoir);
    if ((p.output_size.z & 8u) != 0)
        store_color(colors_a, offset_for(pixel),
                    initial.reservoir.integrand * initial.reservoir.weight);
    if (p.restir.w > 0)
        restir_output[linear] = float4(initial.reservoir.integrand * initial.reservoir.weight, 0);
    restir_residual[linear] = float4(initial.residual, distance);
#ifndef PRIMEPT_SEPARATE_GUIDES
    if (p.reconstruction.w == 2)
    {
        pt::TransportCall data = {};
        data.path.origin = scene.initialOrigin(pixel);
        data.path.direction = scene.initialDirection(pixel);
        data.path.medium = scene.initialMedium(pixel);
        float2 film = (float2(pixel) + 0.5 + p.reconstruction.xy) / float2(p.image.xy);
        pt::DenoiseCamera previous = { p.previous_position, p.previous_forward, p.previous_right,
                                       p.previous_up };
        pt::DenoiseCamera current =
            geometry::camera_from_scaled(p.origin.xyz, p.forward.xyz, p.right.xyz, p.up.xyz);
        Guides guides = { pixel, film, current, previous };
        if (hit.isValid())
        {
            guides.surface_known(hit, data);
        }
        else
            guides.environment(data);
    }
#endif
}

// 准入只恢复几何，不重复翻译材质；两个方向分别加载自己的目标表面。
public bool temporal_donor(uint2 pixel, out uint2 donor)
{
    donor = 0;
    Params p = parameters[0];
    HitInfo current = primary(pixel);
    if (p.temporal_reserved.y == 0 || !current.isValid())
        return false;
    geometry::SurfacePoint point = geometry::surface_point(restir_endpoint(current));
    geometry::Camera camera = { p.previous_position, p.previous_forward, p.previous_right,
                                p.previous_up };
    geometry::CameraProjection projected =
        geometry::camera_project(camera, point.position, p.image.xy);
    if (!temporal_candidate(projected.pixel, projected.valid, p.image.xy, p.reconstruction.xy,
                            donor))
        return false;
    HitInfo previous;
    previous.data = primary_history[offset_for(donor)];
    // donor只须仍能重建；跨面/材质/区块可以作为候选，精确shift负责零支持和失效路径。
    return restir_valid_hit(previous);
}

[shader("raygeneration")]
void restir_temporal_direction()
{
    uint2 pixel = DispatchRaysIndex().xy;
    uint direction = DispatchRaysIndex().z;
    uint offset = offset_for(pixel);
    uint2 donor;
    bool valid = temporal_donor(pixel, donor);
    if (direction == 0)
        neighbor_masks[offset] = uint2(uint(valid), donor.x | (donor.y << 12));
    if (!valid)
        return;
    RestirScene scene = {};
    bool previous = direction == 0;
    uint2 targetPixel = previous ? donor : pixel;
    uint sourceIndex = previous ? offset : offset_for(donor);
    Surface target;
    if (previous)
    {
        HitInfo hit;
        hit.data = primary_history[offset_for(donor)];
        target = scene.loadPreviousPrimary(hit, donor);
    }
    else
        target = current_surface(scene, pixel);
    ShiftSettings settings = {};
    PrecomputedShiftData shift =
        shiftPathLoaded(scene, target, reservoirs_a, reservoirs_b, sourceIndex, !previous,
                        scene.primaryRayFootprint(target, targetPixel, previous), settings, true,
                        true, true, previous);
    store_precomputed(shifts, temporal_address(offset, direction), shift);
}

[shader("compute")]
[numthreads(16, 16, 1)]
void restir_temporal_combine(uint3 id: SV_DispatchThreadID)
{
    Params p = parameters[0];
    if (any(id.xy >= p.image.xy))
        return;
    uint offset = offset_for(id.xy);
    uint2 mask = neighbor_masks[offset];
    if (mask.x == 0)
        return;
    uint2 donor = uint2(mask.y & 0xfffu, mask.y >> 12);
    uint historyOffset = offset_for(donor);
    PathReservoir canonical = load_merge_source(reservoirs_a, offset);
    PathReservoir history = load_merge_source(reservoirs_b, historyOffset);
    PrecomputedShiftData canonicalAtHistory = load_precomputed(shifts, temporal_address(offset, 0));
    PrecomputedShiftData historyAtCanonical = load_precomputed(shifts, temporal_address(offset, 1));
    if ((p.output_size.z & 16u) != 0)
        store_color(colors_a, offset,
                    cv_temporal(canonical.integrand * canonical.weight,
                                load_color(colors_b, historyOffset), canonical, history,
                                canonicalAtHistory, historyAtCanonical, 1.0,
                                float(p.output_size.w)));
    TinyUniformSampleGenerator generator =
        rng_for_pixel(id.xy, p.temporal_reserved.x ^ p.options.x ^ 0x632be59bu);
    MergeSelection selected =
        temporal_selection(canonical, history, canonicalAtHistory, historyAtCanonical, offset,
                           historyOffset, generator.next2D(), float(p.output_size.w));
    PathReservoir result = selected.canonical ? load_selection(reservoirs_a, selected)
                                              : load_selection(reservoirs_b, selected);
    store_reservoir(reservoirs_a, offset, result);
}

[shader("compute")]
[numthreads(16, 16, 1)]
void restir_spatial_work(uint3 id: SV_DispatchThreadID)
{
    Params p = parameters[0];
    uint2 pixel = id.xy;
    if (any(pixel >= p.image.xy))
        return;
    uint offset = offset_for(pixel);
    PathReservoir canonical = load_merge_source(reservoirs_a, offset);
    uint mask = 0;
    float sum = canonical.M;
    PairedNeighborTransform transform = transform_for_frame();
    HitInfo hit = primary(pixel);
    if (hit.isValid())
    {
        geometry::SurfacePoint point = geometry::surface_point(restir_endpoint(hit));
        AdmissionSurface central = reuse_geometry(hit, point);
        for (uint slot = 0; slot < 3; ++slot)
        {
            int2 donor = paired_neighbor(paired_lut, transform, slot, pixel);
            if (!valid_pixel(donor, p.image.xy))
                continue;
            HitInfo neighborHit = primary(uint2(donor));
            if (!neighborHit.isValid())
                continue;
            geometry::SurfacePoint neighbor = geometry::surface_point(restir_endpoint(neighborHit));
            PathReservoir source = load_merge_source(reservoirs_a, offset_for(uint2(donor)));
            if (spatial_candidate(central, reuse_geometry(neighborHit, neighbor)))
            {
                mask |= 1u << slot;
                sum += source.M;
            }
        }
    }
    neighbor_masks[offset] = uint2(mask, asuint(sum));
}

/// 诊断读取同一帧的实际paired地址；不恢复材质或触发额外追踪。
public uint4 cv_spatial_indices(uint2 pixel)
{
    uint offset = offset_for(pixel);
    uint mask = neighbor_masks[offset].x;
    uint4 result = uint4(offset, 0xffffffffu, 0xffffffffu, 0xffffffffu);
    for (uint slot = 0; slot < 3; ++slot)
        if ((mask & (1u << slot)) != 0)
            result[slot + 1u] =
                offset_for(uint2(paired_neighbor(paired_lut, transform_for_frame(), slot, pixel)));
    return result;
}

[shader("raygeneration")]
void restir_spatial_direction()
{
    Params p = parameters[0];
    uint linear = DispatchRaysIndex().x + DispatchRaysDimensions().x * DispatchRaysIndex().y;
    uint pixels = p.image.x * p.image.y;
    uint slot = linear / pixels;
    linear %= pixels;
    uint2 pixel = uint2(linear % p.image.x, linear / p.image.x);
    uint offset = offset_for(pixel);
    if ((neighbor_masks[offset].x & (1u << slot)) == 0)
        return;
    uint2 donor = uint2(paired_neighbor(paired_lut, transform_for_frame(), slot, pixel));
    RestirScene scene = {};
    PrecomputedShiftData shift = shifted(scene, current_surface(scene, pixel), reservoirs_a,
                                         offset_for(donor), pixel, false);
    store_precomputed(shifts, paired_address(offset, slot), shift);
}

[shader("compute")]
[numthreads(16, 16, 1)]
void restir_merge(uint3 id: SV_DispatchThreadID)
{
    Params p = parameters[0];
    if (any(id.xy >= p.image.xy))
        return;
    uint offset = offset_for(id.xy);
    uint2 mask = neighbor_masks[offset];
    bool reuse = mask.x != 0 && asfloat(mask.y) > 0.0;
    // 没有邻居时直接传完整记录；不先读热20B再为相同源回读80B。
    PathReservoir canonical =
        reuse ? load_merge_source(reservoirs_a, offset) : load_reservoir(reservoirs_a, offset);
    PathReservoir result = canonical;
    bool cv = (p.output_size.z & 8u) != 0;
    float3 color = cv ? load_color(colors_a, offset) : float3(0);
    CVAccumulator colors;
    colors.init(color, canonical.M);
    if (reuse)
    {
        SpatialSelection selection;
        selection.init(canonical, asfloat(mask.y));
        PairedNeighborTransform transform = transform_for_frame();
        float3 reflectance = cv ? cv_reflectance(id.xy) : float3(1);
        TinyUniformSampleGenerator generator =
            rng_for_pixel(id.xy, p.temporal_reserved.x ^ p.options.x ^ 0x85157af5u);
        for (uint slot = 0; slot < 3; ++slot)
        {
            float sample = generator.next1D();
            if ((mask.x & (1u << slot)) == 0)
                continue;
            uint2 donor = uint2(paired_neighbor(paired_lut, transform, slot, id.xy));
            uint other = offset_for(donor);
            PathReservoir source = load_merge_source(reservoirs_a, other);
            PrecomputedShiftData forward = load_precomputed(shifts, paired_address(offset, slot));
            PrecomputedShiftData reverse = load_precomputed(shifts, paired_address(other, slot));
            if (cv && source.M > 0)
                colors.add(cv_from_neighbor(load_color(colors_a, other), canonical, source, forward,
                                            reverse, cv_alpha(reflectance, cv_reflectance(donor))),
                           source.M);
            selection.add(source, other, forward, reverse, sample);
        }
        selection.finish(offset, generator.next1D());
        result = load_selection(reservoirs_a, selection.selected);
    }
    store_reservoir(reservoirs_b, offset, result);
    primary_history[offset] = primary_current[offset];
    if (cv)
    {
        color = colors.resolve();
        store_color(colors_b, offset, color);
    }
    float3 radiance = cv ? color : result.integrand * result.weight;
    if (fixed_mixture_fresh(id.xy, p.temporal_reserved.x + p.options.x, p.restir.w))
        radiance = restir_output[id.y * p.image.x + id.x].xyz;
    restir_output[id.y * p.image.x + id.x] =
        all(isfinite(radiance)) ? float4(radiance, 0) : float4(0, 0, 0, -1);
}
