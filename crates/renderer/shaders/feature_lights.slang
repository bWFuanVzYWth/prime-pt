/// 顶层灯树只消费同帧设备数据。静态区域直接复用已发布的根，动态面按姿态后的实际顶点计量。
import world_features;
import light_bvh.world;
import light_bvh.radix;
import light_bvh.order;
import light_sampling.area;

[[vk::binding(0, 0)]]
var config : StructuredBuffer<uint>;
[[vk::binding(1, 0)]]
var surfaces : RWStructuredBuffer<world_features::Surface>;
[[vk::binding(2, 0)]]
var tree : RWStructuredBuffer<uint4>;
[[vk::binding(3, 0)]]
var terrain : StructuredBuffer<light_bvh::LightWorld>;
[[vk::binding(4, 0)]]
var args : RWStructuredBuffer<uint4>;
[[vk::binding(5, 0)]]
var merge_width : StructuredBuffer<uint>;

static const uint leaf_bit = 0x80000000u;
func group_start(group: uint3, size: uint)->uint
{
    return (group.x + 65535u * group.y) * size;
}
func ordered(value: float3)->uint3
{
    let bits = asuint(value);
    return bits ^ (asuint(asint(bits) >> 31) | 0x80000000u);
}
func decode_ordered(value: uint3)->float3
{
    return asfloat(value ^
                   select((value & 0x80000000u) != 0u, uint3(0x80000000u), uint3(0xffffffffu)));
}

[shader("compute")]
[numthreads(128, 1, 1)]
func feature_lights_collect(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let id = group_start(group, 128u) + lane;
    let regions = config[1];
    let region = id < regions;
    let slot = id - regions;
    var emitting = false;
    if (!region && slot < config[0])
    {
        emitting = (surfaces[slot].material.y & 4u) != 0u;
    }
    let wave_count = WaveActiveCountBits(emitting);
    let local_rank = WavePrefixCountBits(emitting);
    var first = 0u;
    if (WaveIsFirstLane() && wave_count != 0u)
        InterlockedAdd(args[0].x, wave_count, first);
    first = WaveReadLaneFirst(first);
    let rank = region ? id : regions + first + local_rank;
    var metric = float4(0);
    var meta = uint4(0);
    if (region)
    {
        let anchor = asint(uint3(config[4], config[5], config[6]));
        metric = light_bvh::region_summary(terrain[0].regions[id], anchor);
        meta.w = leaf_bit | id;
    }
    else if (emitting)
    {
        let a = surfaces[slot].position[0].xyz;
        let b = surfaces[slot].position[1].xyz;
        let c = surfaces[slot].position[2].xyz;
        let d = surfaces[slot].position[3].xyz;
        let vertices : light_sampling::LightQuadVertices = { a, b, c, d };
        var double_area : float2;
        let area = light_sampling::dynamic_quad_area(vertices, double_area);
        let total = double_area.x + double_area.y;
        if (total > 0 && isfinite(total))
        {
            let gain = (surfaces[slot].material.y & 256u) != 0 ? surfaces[slot].previous[1].w : 1.0;
            let tint = (world_features::emitter_tint(asuint(surfaces[slot].position[0].w)) +
                        world_features::emitter_tint(asuint(surfaces[slot].position[1].w)) +
                        world_features::emitter_tint(asuint(surfaces[slot].position[2].w)) +
                        world_features::emitter_tint(asuint(surfaces[slot].position[3].w))) *
                       0.25;
            // 双面因子与三角形面积中的 1/2 抵消。保留受遮罩亮点的提案支持。
            let power = total * gain * max(tint, 1.0e-6);
            metric = float4((a + c + lerp(d, b, double_area.x / total)) / 3.0, power);
        }
        meta = uint4(area.xyz, slot);
    }
    let active = region || emitting;
    if (active)
    {
        tree[4u * config[2] + rank] = asuint(metric);
        tree[5u * config[2] + rank] = meta;
    }
    let bits = ordered(metric.xyz);
    let lo = WaveActiveMin(active ? bits : uint3(0xffffffffu));
    let hi = WaveActiveMax(active ? bits : uint3(0));
    if (WaveIsFirstLane())
    {
        [unroll]
        for (var k = 0u; k < 3u; k++)
        {
            InterlockedMax(args[1][k], ~lo[k]);
            InterlockedMax(args[2][k], hi[k]);
        }
    }
}

func dispatch_args(groups: uint)->uint4
{
    return uint4(min(groups, 65535u), (groups + 65534u) / 65535u, 1, 0);
}

// 固定的各级参数行允许 CPU 预录上界。实际没有工作的级别在设备上写为零组。
groupshared uint collected_count;
[shader("compute")]
[numthreads(32, 1, 1)]
func feature_lights_schedule(lane: uint: SV_GroupIndex)->void
{
    if (lane == 0u)
        collected_count = args[0].x;
    GroupMemoryBarrierWithGroupSync();
    let dynamic_count = collected_count;
    let n = dynamic_count == 0u ? 0u : config[1] + dynamic_count;
    let exponent = n <= 1u ? 0u : uint(firstbithigh(n - 1u)) + 1u;
    let leaves = 1u << exponent;
    let passes = uint(max(int(exponent) - 7, 0));
    if (lane == 0u)
    {
        args[0].yzw = uint3(n, leaves, passes & 1u);
        let lo = decode_ordered(~args[1].xyz);
        let hi = decode_ordered(args[2].xyz);
        let extent = hi - lo;
        args[3] = asuint(
            float4(lo, min(2.0, 1048575.0 / max(1.0, max(extent.x, max(extent.y, extent.z))))));
        tree[0] = uint4(n, leaves, leaf_bit, min(terrain[0].counts.x, 1u << 24u) + dynamic_count);
    }
    if (lane == 4u)
        args[lane] = dispatch_args((n + 127u) / 128u);
    if (lane >= 5u && lane < 22u)
        args[lane] = dispatch_args(lane - 5u < passes ? (n + 127u) / 128u : 0u);
    if (lane >= 22u && lane < 26u)
    {
        let level = lane - 22u;
        let width = leaves >> (level * 6u);
        let active = n != 0u && (level == 0u || exponent > level * 6u);
        args[lane] = dispatch_args(active ? (width + 63u) / 64u : 0u);
    }
    if (lane == 26u)
        args[lane] = dispatch_args(n > 1u ? (n + 63u) / 64u : 0u);
}
[shader("compute")]
[numthreads(128, 1, 1)]
func feature_lights_sort(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let i = group_start(group, 128u) + lane;
    let n = args[0].y;
    let capacity = config[2];
    var key = uint4(0xffffffffu);
    if (i < n)
    {
        let box = asfloat(args[3]);
        let q =
            uint3(clamp((asfloat(tree[4u * capacity + i]).xyz - box.xyz) * box.w, 0.0, 1048575.0));
        key = uint4(light_bvh::morton10(q >> 10u), light_bvh::morton10(q),
                    tree[5u * capacity + i].w, i);
    }
    key = light_bvh::sort_tile(key, lane);
    if (i < n)
        tree[7u * capacity + i] = key;
}
[shader("compute")]
[numthreads(128, 1, 1)]
func feature_lights_merge(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let block = group_start(group, 128u);
    let n = args[0].y;
    let width = merge_width[0];
    let bank = (uint(firstbithigh(width)) - 7u) & 1u;
    let src = (7u + bank) * config[2];
    let dest = (8u - bank) * config[2];
    light_bvh::merge_tile(tree, src, dest, n, width, block, lane);
}
groupshared float4 tile[64];
groupshared uint tile_support[64];

func build_tile<let stride : uint>(group: uint3, lane: uint)->void
{
    let head = args[0];
    let n = head.y;
    let leaves = head.z;
    let capacity = config[2];
    let sorted = (7u + head.w) * capacity;
    let support_base = (8u - head.w) * capacity;
    let width = leaves / stride;
    let first = group_start(group, 64u);
    if (first >= width)
        return;
    let i = first + lane;
    var metric = float4(0);
    var support = 0u;
    if (stride == 1u)
    {
        if (i < n)
        {
            let raw = tree[sorted + i].w;
            let meta = tree[5u * capacity + raw];
            metric = asfloat(tree[4u * capacity + raw]);
            tree[2u * leaves + i] = meta;
            if ((meta.w & leaf_bit) != 0u)
            {
                let region = meta.w & ~leaf_bit;
                support = terrain[0].regions[region].count;
                tree[3u * leaves + region] = uint4(i, 0, 0, 0);
            }
            else
            {
                support = 1u;
                surfaces[meta.w].previous[2].w = asfloat(i + 1u);
            }
            tree[sorted + i].z = support;
        }
        if (i < width)
            tree[width + i] = asuint(metric);
    }
    else if (i < width)
    {
        metric = asfloat(tree[width + i]);
        support = tree[support_base + width + i].x;
    }
    tile[lane] = metric;
    tile_support[lane] = support;
    for (var step = 1u; step < min(width, 64u); step <<= 1u)
    {
        GroupMemoryBarrierWithGroupSync();
        if (i < width && (lane & (2u * step - 1u)) == 0u)
        {
            let merged = light_bvh::merge_nodes(tile[lane], tile[lane + step]);
            let total = tile_support[lane] + tile_support[lane + step];
            tile[lane] = merged;
            tile_support[lane] = total;
            let node = (width + i) / (2u * step);
            tree[node] = asuint(merged);
            tree[support_base + node].x = total;
        }
    }
}
[shader("compute")]
[numthreads(64, 1, 1)]
func feature_lights_1(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    build_tile<1>(group, lane);
}

[shader("compute")]
[numthreads(64, 1, 1)]
func feature_lights_64(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    build_tile<64>(group, lane);
}

[shader("compute")]
[numthreads(64, 1, 1)]
func feature_lights_4096(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    build_tile<4096>(group, lane);
}

[shader("compute")]
[numthreads(64, 1, 1)]
func feature_lights_262144(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    build_tile<262144>(group, lane);
}

[shader("compute")]
[numthreads(64, 1, 1)]
func feature_lights_radix(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let mid = group_start(group, 64u) + lane;
    let head = args[0];
    let n = head.y;
    let leaves = head.z;
    let sorted = (7u + head.w) * config[2];
    let supports = (8u - head.w) * config[2];
    let total = min(terrain[0].counts.x, 1u << 24u) + head.x;
    light_bvh::build_radix_branch(tree, mid, n, leaves, sorted, supports, total);
}
