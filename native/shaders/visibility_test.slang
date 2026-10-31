/// 同一场景的 TraceRay 与 RayQuery 覆盖一致性探针，只供无窗口测试。
module visibility_test;
import mod_scene;
import pt.scene;
import geometry.ray;

[[vk::binding(4, 0)]]
RWStructuredBuffer<uint> result;

[shader("raygeneration")]
void reference_main()
{
    uint index = DispatchRaysIndex().x;
    ModScene provider;
    pt::Hit hit;
    bool found = provider.intersect(float3(0), float3(0, 0, 1), hit);
    result[index] = uint(found && hit.distance < (index == 0 ? 1.5 : 3.0));
}

[shader("compute")]
[numthreads(2, 1, 1)]
void visibility_main(uint3 id: SV_DispatchThreadID)
{
    geometry::Ray ray = { float3(0), 0, float3(0, 0, 1), id.x == 0 ? 1.5 : 3.0 };
    ModScene provider;
    result[2 + id.x] = uint(provider.occluded(ray));
}
