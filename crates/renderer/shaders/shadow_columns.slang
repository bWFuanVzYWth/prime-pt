import atmosphere.shadow_resolve;
import atmosphere.terrain_frame;
using namespace atmosphere;
[[vk::binding(0, 0)]]
StructuredBuffer<TerrainFrame> frames;
[[vk::binding(1, 0)]]
RWStructuredBuffer<uint4> cells;
[[vk::binding(2, 0)]]
RaytracingAccelerationStructure world;
[[vk::binding(3, 0)]]
RWStructuredBuffer<uint> first_hits;
[[vk::binding(4, 0)]]
StructuredBuffer<uint4> columns;
import mod_coverage;
[[vk::binding(5, 0)]]
StructuredBuffer<uint> bases;
[[vk::binding(6, 0)]]
Texture2D<float4> atlas;
[[vk::binding(7, 0)]]
ByteAddressBuffer materials;
struct Coverage : IBlockerCoverage
{
    static bool accepts(uint instance, uint primitive, float2 bary, bool front)
    {
        return prime_coverage::accepts<40>(columns, atlas, materials,
                                           bases[instance] * 2 + primitive, bary, front);
    }
};
[shader("compute")]
[numthreads(8, 8, 1)]
void shadow_columns(uint3 id: SV_DispatchThreadID)
{
    resolve_shadow_column<Coverage>(id, frames[0], cells, world, first_hits);
}
