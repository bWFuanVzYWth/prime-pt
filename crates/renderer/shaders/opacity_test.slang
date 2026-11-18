/// 与生产覆盖适配器共用的 TraceRay/RayQuery OMM 回归。
module opacity_test;
import mod_scene;
import pt.scene;
import geometry.ray;
#include "opacity/micromap.inc.slang"
[[vk::binding(4, 0)]]
var result : RWStructuredBuffer<uint>;
func probe(index: uint)->geometry::Ray
{
    let lane : uint = index % 16;
    let front : bool = lane >= 8;
    return { float3(float(index >> 4) * 3.0 - 0.75 + float(lane % 4) * 0.5,
                    ((lane >> 2) % 2) == 0 ? -0.6 : 0.6, front ? 2.0 : 0.0),
             0, float3(0, 0, front ? -1.0 : 1.0), 1.5 };
}
[shader("raygeneration")]
func trace_main()->void
{
    let index : uint = DispatchRaysIndex().x;
    let ray : geometry::Ray = probe(index);
    var hit : pt::Hit;
    result[index] =
        uint(ModScene().intersect(ray.origin, ray.direction, hit) && hit.distance < ray.t_max);
}
[shader("compute")]
[numthreads(64, 1, 1)]
func query_main(id: uint3: SV_DispatchThreadID)->void
{
    ENGINE_OPACITY_MICROMAP(query_main)
    result[64 + id.x] = uint(ModScene().occluded(probe(id.x)));
}
