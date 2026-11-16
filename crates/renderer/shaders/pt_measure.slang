/// 实验读回适配：计时区间之外累积HDR和小块统计，避免逐帧PCIe回读改变功耗状态。
module spatial_measure;

[[vk::binding(0, 0)]]
var source : StructuredBuffer<float4>;
[[vk::binding(1, 0)]]
var sums : RWStructuredBuffer<float4>;
[[vk::binding(2, 0)]]
var blocks : RWStructuredBuffer<float4>;
/// xy尺寸，z从零开始的帧号；blocks保留最近32帧。
[[vk::binding(3, 0)]]
var config : StructuredBuffer<uint4>;

groupshared var reduced : float4[256];

[shader("compute")]
[numthreads(16, 16, 1)]
func measure(id: uint3: SV_DispatchThreadID, group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)
    ->void
{
    let extent : uint2 = config[0].xy;
    let frame : uint = config[0].z;
    var value = float4(0);
    if (all(id.xy < extent))
    {
        let index : uint = id.y * extent.x + id.x;
        value = source[index];
        sums[index] = (frame == 0 ? float4(0) : sums[index]) + value;
    }
    reduced[lane] = float4(value.xyz, all(isfinite(value)) && value.w >= 0
                                          ? max(abs(value.x), max(abs(value.y), abs(value.z)))
                                          : asfloat(0x7f800000u));
    GroupMemoryBarrierWithGroupSync();
    for (var stride: uint = 128; stride > 0; stride >>= 1)
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
        let tiles : uint2 = (extent + 15u) >> 4u;
        let size : uint2 = min(uint2(16), extent - group.xy * 16u);
        blocks[((frame % 32u) * tiles.y + group.y) * tiles.x + group.x] =
            float4(reduced[0].xyz / float(size.x * size.y), reduced[0].w);
    }
}
