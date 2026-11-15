/// 实验读回适配：计时区间之外累积HDR和小块统计，避免逐帧PCIe回读改变功耗状态。
module spatial_measure;

[[vk::binding(0, 0)]]
StructuredBuffer<float4> source;
[[vk::binding(1, 0)]]
RWStructuredBuffer<float4> sums;
[[vk::binding(2, 0)]]
RWStructuredBuffer<float4> blocks;
/// xy尺寸，z从零开始的帧号；blocks保留最近32帧。
[[vk::binding(3, 0)]]
StructuredBuffer<uint4> config;

groupshared float4 reduced[256];

[shader("compute")]
[numthreads(16, 16, 1)]
void measure(uint3 id: SV_DispatchThreadID, uint3 group: SV_GroupID, uint lane: SV_GroupIndex)
{
    uint2 extent = config[0].xy;
    uint frame = config[0].z;
    float4 value = float4(0);
    if (all(id.xy < extent))
    {
        uint index = id.y * extent.x + id.x;
        value = source[index];
        sums[index] = (frame == 0 ? float4(0) : sums[index]) + value;
    }
    reduced[lane] = float4(value.xyz, all(isfinite(value)) && value.w >= 0
                                          ? max(abs(value.x), max(abs(value.y), abs(value.z)))
                                          : asfloat(0x7f800000u));
    GroupMemoryBarrierWithGroupSync();
    for (uint stride = 128; stride > 0; stride >>= 1)
    {
        if (lane < stride)
        {
            reduced[lane].xyz += reduced[lane + stride].xyz;
            reduced[lane].w = max(reduced[lane].w, reduced[lane + stride].w);
        }
        GroupMemoryBarrierWithGroupSync();
    }
    if (lane == 0)
    {
        uint2 tiles = (extent + 15u) >> 4u;
        uint2 size = min(uint2(16), extent - group.xy * 16u);
        blocks[((frame % 32u) * tiles.y + group.y) * tiles.x + group.x] =
            float4(reduced[0].xyz / float(size.x * size.y), reduced[0].w);
    }
}
