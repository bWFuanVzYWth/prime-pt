/// 顶层灯树只消费同帧设备数据。静态区域直接复用已发布的根，动态面按姿态后的实际顶点计量。
import world_features;
import light_bvh.world;
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
        let area = light_sampling::dynamic_quad_area(vertices);
        let first_area = length(cross(b - a, c - a));
        let second_area = length(cross(c - a, d - a));
        let total = first_area + second_area;
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
            metric = float4((a + c + lerp(d, b, first_area / total)) / 3.0, power);
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
groupshared uint4 keys[128];
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
    keys[lane] = key;
    GroupMemoryBarrierWithGroupSync();
    [unroll]
    for (var width = 2u; width <= 128u; width <<= 1u)
    {
        [unroll]
        for (var step = width >> 1u; step > 0u; step >>= 1u)
        {
            let peer = lane ^ step;
            if (lane < peer)
            {
                let a = keys[lane];
                let b = keys[peer];
                if ((lane & width) == 0u ? light_bvh::less_key(b, a) : light_bvh::less_key(a, b))
                {
                    keys[lane] = b;
                    keys[peer] = a;
                }
            }
            GroupMemoryBarrierWithGroupSync();
        }
    }
    if (i < n)
        tree[7u * capacity + i] = keys[lane];
}
// 在两条有序序列中定位一个输出边界。每块只做两次全局二分，组内归并读取共享内存。
func left_cut(src: uint, base: uint, left: uint, right: uint, total: uint)->uint
{
    var lo = total - min(total, right);
    var hi = min(total, left);
    while (lo < hi)
    {
        let m = (lo + hi) >> 1u;
        let j = total - m;
        if (light_bvh::less_key(tree[src + base + m], tree[src + base + left + j - 1u]))
            lo = m + 1u;
        else
            hi = m;
    }
    return lo;
}
groupshared uint cuts[2];
[shader("compute")]
[numthreads(128, 1, 1)]
func feature_lights_merge(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let block = group_start(group, 128u);
    let n = args[0].y;
    if (block >= n)
        return;
    let width = merge_width[0];
    let base = block & ~(2u * width - 1u);
    let left = min(width, n - base);
    let right = min(width, n - base - left);
    let bank = (uint(firstbithigh(width)) - 7u) & 1u;
    let src = (7u + bank) * config[2];
    let dest = (8u - bank) * config[2];
    let size = min(128u, n - block);
    if (right == 0u)
    {
        if (lane < size)
            tree[dest + block + lane] = tree[src + block + lane];
        return;
    }
    let relative = block - base;
    if (lane < 2u)
        cuts[lane] = left_cut(src, base, left, right, relative + lane * size);
    GroupMemoryBarrierWithGroupSync();
    let count_a = cuts[1] - cuts[0];
    let in_a = lane < count_a;
    var key = uint4(0xffffffffu);
    if (lane < size)
    {
        let index = in_a ? cuts[0] + lane : left + relative - cuts[0] + lane - count_a;
        key = tree[src + base + index];
        keys[lane] = key;
    }
    GroupMemoryBarrierWithGroupSync();
    if (lane >= size)
        return;
    var lo = in_a ? count_a : 0u;
    var hi = in_a ? size : count_a;
    while (lo < hi)
    {
        let mid = (lo + hi) >> 1u;
        if (light_bvh::less_key(keys[mid], key))
            lo = mid + 1u;
        else
            hi = mid;
    }
    tree[dest + block + lane + lo - count_a] = key;
}
func merge_nodes(a: float4, b: float4)->float4
{
    let power = a.w + b.w;
    return power > 0.0 ? float4(lerp(a.xyz, b.xyz, b.w / power), power) : float4(0);
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
            let merged = merge_nodes(tile[lane], tile[lane + step]);
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

struct Interval
{
    float4 metric;
    uint support;
};
func accumulate(inout interval: Interval, node: uint, leaves: uint, sorted: uint, supports: uint)
    ->void
{
    interval.metric = merge_nodes(interval.metric, asfloat(tree[node]));
    interval.support += node >= leaves ? tree[sorted + node - leaves].z : tree[supports + node].x;
}
func interval_value(begin: uint, end: uint, leaves: uint, sorted: uint, supports: uint)->Interval
{
    var lo = begin + leaves;
    var hi = end + leaves;
    var interval : Interval = { float4(0), 0u };
    while (lo < hi)
    {
        if ((lo & 1u) != 0u)
        {
            accumulate(interval, lo, leaves, sorted, supports);
            lo++;
        }
        if ((hi & 1u) != 0u)
        {
            hi--;
            accumulate(interval, hi, leaves, sorted, supports);
        }
        lo >>= 1u;
        hi >>= 1u;
    }
    return interval;
}
// 共同前缀决定 radix 子树；空间码相同时补入唯一有序叶号。
func prefix(sorted: uint, a: uint, b: uint)->int
{
    let diff = tree[sorted + a].xy ^ tree[sorted + b].xy;
    if (diff.x != 0u)
        return 31 - firstbithigh(diff.x);
    if (diff.y != 0u)
        return 63 - firstbithigh(diff.y);
    return a == b ? 96 : 95 - firstbithigh(a ^ b);
}
func child(sorted: uint, lo: uint, hi: uint)->uint
{
    if (hi == lo + 1u)
        return leaf_bit | lo;
    let depth = prefix(sorted, lo, hi - 1u);
    var a = lo + 1u;
    var b = hi - 1u;
    while (a < b)
    {
        let m = (a + b) >> 1u;
        if (prefix(sorted, lo, m) > depth)
            a = m + 1u;
        else
            b = m;
    }
    return a;
}
// 每个相邻分界对应唯一内节点。指数搜索仅访问其子树邻域，各工作组间无自旋。
[shader("compute")]
[numthreads(64, 1, 1)]
func feature_lights_radix(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let mid = group_start(group, 64u) + lane;
    let head = args[0];
    let n = head.y;
    let leaves = head.z;
    if (mid == 0u || mid >= n)
        return;
    let sorted = (7u + head.w) * config[2];
    let supports = (8u - head.w) * config[2];
    let depth = prefix(sorted, mid - 1u, mid);
    var step = 2u;
    while (step <= mid && prefix(sorted, mid - step, mid) >= depth)
        step *= 2u;
    var a = mid - min(mid, step - 1u);
    var b = mid - step / 2u;
    while (a < b)
    {
        let m = (a + b) >> 1u;
        if (prefix(sorted, m, mid) >= depth)
            b = m;
        else
            a = m + 1u;
    }
    let lo = a;
    step = 2u;
    while (step <= n - mid && prefix(sorted, mid - 1u, mid + step - 1u) >= depth)
        step *= 2u;
    a = mid + step / 2u;
    b = mid + min(n - mid, step - 1u);
    while (a < b)
    {
        let m = (a + b + 1u) >> 1u;
        if (prefix(sorted, mid - 1u, m - 1u) >= depth)
            a = m;
        else
            b = m - 1u;
    }
    let hi = a;
    let at = 4u * leaves + 3u * mid;
    let left = interval_value(lo, mid, leaves, sorted, supports);
    let right = interval_value(mid, hi, leaves, sorted, supports);
    let total = min(terrain[0].counts.x, 1u << 24u) + head.x;
    let large = total > (1u << 24u);
    tree[at] = asuint(left.metric);
    tree[at + 1u] = asuint(right.metric);
    tree[at + 2u] = uint4(child(sorted, lo, mid), child(sorted, mid, hi),
                          large ? mid - lo : left.support, large ? hi - mid : right.support);
    if (lo == 0u && hi == n)
        tree[0] = uint4(n, leaves, mid, total);
}
