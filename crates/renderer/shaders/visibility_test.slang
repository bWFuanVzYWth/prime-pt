/// 同一场景的 TraceRay 与 RayQuery 覆盖一致性探针，只供无窗口测试。
module visibility_test;
import mod_scene;
import pt.scene;
import geometry.ray;

[[vk::binding(4, 0)]]
var result : RWStructuredBuffer<uint>;

[shader("raygeneration")]
func reference_main()->void
{
    let index : uint = DispatchRaysIndex().x;
    var provider : ModScene;
    var hit : pt::Hit;
    let found : bool = provider.intersect(float3(0), float3(0, 0, 1), hit);
    result[index] = uint(found && hit.distance < (index == 0 ? 1.5 : 3.0));
}

[shader("compute")]
[numthreads(2, 1, 1)]
func visibility_main(id: uint3: SV_DispatchThreadID)->void
{
    let ray : geometry::Ray = { float3(0), 0, float3(0, 0, 1), id.x == 0 ? 1.5 : 3.0 };
    var provider : ModScene;
    result[2 + id.x] = uint(provider.occluded(ray));
}
