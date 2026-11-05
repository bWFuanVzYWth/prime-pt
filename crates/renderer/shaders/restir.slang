/// point ReSTIR PT：融合初采、双向时间shift、paired三邻居空间shift和最终合并。
module restir;
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
/// 与普通PT互斥使用landing normal plane；只在本帧initial写入、merge读取，不跨帧保存。
[[vk::binding(11, 0)]]
RWStructuredBuffer<float4> cv_proxy;
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

float3 load_color(RWByteAddressBuffer buffer, uint offset)
{
    return asfloat(buffer.Load3(12u * offset));
}

void store_color(RWByteAddressBuffer buffer, uint offset, float3 color)
{
    buffer.Store3(12u * offset, asuint(color));
}

// 与game相同，系数只依赖固定primary的UV/tint纹理代理，不读取随机路径辐射。
float3 cv_reflectance(uint2 pixel)
{
    return cv_proxy[pixel.y * parameters[0].image.x + pixel.x].xyz;
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

PrecomputedShiftData shifted(RestirScene scene, Surface destination, PathReservoir source,
                             uint2 pixel, bool previous)
{
    ShiftSettings settings;
    PrecomputedShiftData result = {};
    result.pathThroughput = shiftPath(scene, destination, source,
                                      scene.primaryRayFootprint(destination, pixel, previous),
                                      settings, true, true, true, previous, result.Jacobian);
    result.reconnectionJacobian = source.reconnectionJacobian;
    return result;
}

[shader("raygeneration")]
void restir_initial()
{
    uint2 pixel = DispatchRaysIndex().xy;
    Params p = parameters[0];
    uint linear = pixel.y * p.image.x + pixel.x;
    RestirScene scene;
    HitInfo hit;
    float distance;
    scene.trace(scene.initialOrigin(pixel), scene.initialDirection(pixel), false, hit, distance);
    primary_current[offset_for(pixel)] = hit.data;
    if ((p.output_size.z & 8u) != 0)
    {
        float3 proxy = hit.isValid()
                           ? max(material_texel(hit.data.x - 1u, asfloat(hit.data.yz)).xyz, 0.04)
                           : float3(0.04);
        cv_proxy[linear] = float4(proxy, 0);
    }
    TinyUniformSampleGenerator generator =
        rng_for_pixel(pixel, p.temporal_reserved.x ^ p.options.x);
    ShiftSettings settings;
    InitialResult initial = sampleInitialPath<RestirScene, 2>(
        scene, pixel, hit, generator.state, p.options.y, settings, true, true, true);
    store_reservoir(reservoirs_a, offset_for(pixel), initial.reservoir);
    if ((p.output_size.z & 8u) != 0)
        store_color(colors_a, offset_for(pixel),
                    initial.reservoir.integrand * initial.reservoir.weight);
    if (p.restir.w > 0)
        restir_output[linear] = float4(initial.reservoir.integrand * initial.reservoir.weight, 0);
    restir_residual[linear] = float4(initial.residual, distance);
    if (p.reconstruction.w > 0)
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
            data.hit = restir_hit(current_surface(scene, pixel));
            guides.surface(data);
        }
        else
            guides.environment(data);
    }
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
    // 静止点的投影含当前射线jitter；扣当前值保持同像素donor，前帧偏移只用于重放。
    float2 position = projected.pixel - p.reconstruction.xy;
    if (!projected.valid || any(position < 0) || any(position >= float2(p.image.xy)))
        return false;
    donor = uint2(position);
    HitInfo previous;
    previous.data = primary_history[offset_for(donor)];
    if (!restir_valid_hit(previous) || previous.data.x != current.data.x ||
        previous.data.w != current.data.w)
        return false;
    geometry::SurfacePoint old = geometry::surface_point(restir_endpoint(previous));
    return paired_geometry_valid(point.normal, length(point.position - p.previous_position.xyz),
                                 old.normal, length(old.position - p.previous_position.xyz));
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
    RestirScene scene;
    PathReservoir canonical = load_reservoir(reservoirs_a, offset);
    PathReservoir history = load_reservoir(reservoirs_b, offset_for(donor));
    PrecomputedShiftData shift = {};
    if (canonical.M > 0 && history.M > 0)
    {
        if (direction == 0)
        {
            HitInfo hit;
            hit.data = primary_history[offset_for(donor)];
            Surface target =
                scene.loadSurfaceFromOrigin(hit, parameters[0].previous_position.xyz, true);
            shift = shifted(scene, target, canonical, donor, true);
        }
        else
        {
            history.M = min(history.M, float(parameters[0].output_size.w));
            shift = shifted(scene, current_surface(scene, pixel), history, pixel, false);
        }
    }
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
    PathReservoir canonical = load_reservoir(reservoirs_a, offset);
    PathReservoir history = load_reservoir(reservoirs_b, offset_for(donor));
    if (canonical.M <= 0 || history.M <= 0)
        return;
    history.M = min(history.M, float(p.output_size.w));
    PrecomputedShiftData canonicalAtHistory = load_precomputed(shifts, temporal_address(offset, 0));
    PrecomputedShiftData historyAtCanonical = load_precomputed(shifts, temporal_address(offset, 1));
    if ((p.output_size.z & 16u) != 0)
        store_color(colors_a, offset,
                    cv_temporal(canonical.integrand * canonical.weight,
                                load_color(colors_b, offset_for(donor)), canonical, history,
                                canonicalAtHistory, historyAtCanonical, 1.0,
                                float(p.output_size.w)));
    TinyUniformSampleGenerator generator =
        rng_for_pixel(id.xy, p.temporal_reserved.x ^ p.options.x ^ 0x632be59bu);
    store_reservoir(reservoirs_a, offset,
                    temporal_resample(canonical, history, canonicalAtHistory, historyAtCanonical,
                                      generator.next2D(), float(p.output_size.w)));
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
    PathReservoir canonical = load_reservoir(reservoirs_a, offset);
    uint mask = 0;
    float sum = canonical.M;
    PairedNeighborTransform transform = transform_for_frame();
    HitInfo hit = primary(pixel);
    if (hit.isValid())
    {
        geometry::SurfacePoint point = geometry::surface_point(restir_endpoint(hit));
        float distance = length(p.origin.xyz - point.position);
        for (uint slot = 0; slot < 3; ++slot)
        {
            int2 donor = paired_neighbor(paired_lut, transform, slot, pixel);
            if (!valid_pixel(donor, p.image.xy))
                continue;
            HitInfo neighborHit = primary(uint2(donor));
            if (!neighborHit.isValid())
                continue;
            geometry::SurfacePoint neighbor = geometry::surface_point(restir_endpoint(neighborHit));
            PathReservoir source = load_reservoir(reservoirs_a, offset_for(uint2(donor)));
            if (source.M > 0 && paired_geometry_valid(point.normal, distance, neighbor.normal,
                                                      length(p.origin.xyz - neighbor.position)))
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
    RestirScene scene;
    PrecomputedShiftData shift =
        shifted(scene, current_surface(scene, pixel),
                load_reservoir(reservoirs_a, offset_for(donor)), pixel, false);
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
    PathReservoir canonical = load_reservoir(reservoirs_a, offset);
    uint2 mask = neighbor_masks[offset];
    PathReservoir result = canonical;
    bool cv = (p.output_size.z & 8u) != 0;
    float3 color = cv ? load_color(colors_a, offset) : float3(0);
    CVAccumulator colors;
    colors.init(color, canonical.M);
    if (canonical.M > 0 && mask.x != 0)
    {
        PairwiseFactor factor = { asfloat(mask.y), canonical.M };
        float canonicalWeight = spatial_begin(factor);
        result = empty_reservoir();
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
            PathReservoir source = load_reservoir(reservoirs_a, other);
            PrecomputedShiftData forward = load_precomputed(shifts, paired_address(offset, slot));
            PrecomputedShiftData reverse = load_precomputed(shifts, paired_address(other, slot));
            if (cv && source.M > 0)
                colors.add(cv_from_neighbor(load_color(colors_a, other), canonical, source, forward,
                                            reverse, cv_alpha(reflectance, cv_reflectance(donor))),
                           source.M);
            spatial_merge_neighbor(result, canonical, source, forward, reverse, factor,
                                   canonicalWeight, sample);
        }
        spatial_finish(result, canonical, canonicalWeight, generator.next1D());
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
