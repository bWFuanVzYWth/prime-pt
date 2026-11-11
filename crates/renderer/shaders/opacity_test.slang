/// 与生产覆盖适配器共用的 TraceRay/RayQuery OMM 回归。
module opacity_test;
import mod_scene;
import pt.scene;
import geometry.ray;
#include "geometry/opacity.slang"
[[vk::binding(4, 0)]]
RWStructuredBuffer<uint> result;
geometry::Ray probe(uint index)
{
    uint lane = index % 16;
    bool front = lane >= 8;
    return { float3(float(index / 16) * 3.0 - 0.75 + float(lane % 4) * 0.5,
                    (lane / 4) % 2 == 0 ? -0.6 : 0.6, front ? 2.0 : 0.0),
             0, float3(0, 0, front ? -1.0 : 1.0), 1.5 };
}
[shader("raygeneration")]
void trace_main()
{
    uint index = DispatchRaysIndex().x;
    geometry::Ray ray = probe(index);
    pt::Hit hit;
    result[index] =
        uint(ModScene().intersect(ray.origin, ray.direction, hit) && hit.distance < ray.t_max);
}
[shader("compute")]
[numthreads(64, 1, 1)]
void query_main(uint3 id: SV_DispatchThreadID)
{
    ENGINE_OPACITY_MICROMAP(query_main)
    result[64 + id.x] = uint(ModScene().occluded(probe(id.x)));
}
