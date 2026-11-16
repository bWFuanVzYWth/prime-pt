import atmosphere.shadow_resolve;
import atmosphere.terrain_frame;
using namespace atmosphere;
[[vk::binding(0, 0)]]
var frames : StructuredBuffer<TerrainFrame>;
[[vk::binding(1, 0)]]
var cells : RWStructuredBuffer<uint4>;
[[vk::binding(2, 0)]]
var world : RaytracingAccelerationStructure;
[[vk::binding(3, 0)]]
var first_hits : RWStructuredBuffer<uint>;
[[vk::binding(4, 0)]]
var columns : StructuredBuffer<uint4>;
import mod_coverage;
[[vk::binding(5, 0)]]
var bases : StructuredBuffer<uint>;
[[vk::binding(6, 0)]]
var atlas : Texture2D<float4>;
[[vk::binding(7, 0)]]
var materials : ByteAddressBuffer;
struct Coverage : IBlockerCoverage
{
    static func accepts(instance: uint, primitive: uint, bary: float2, front: bool)->bool
    {
        return prime_coverage::accepts<40>(columns, atlas, materials,
                                           bases[instance] * 2 + primitive, bary, front);
    }
};
[shader("compute")]
[numthreads(8, 8, 1)]
func shadow_columns(id: uint3: SV_DispatchThreadID)->void
{
    resolve_shadow_column<Coverage>(id, frames[0], cells, world, first_hits);
}
