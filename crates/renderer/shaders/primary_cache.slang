/// 本帧首命中32B；只在实时普通PT中使用。几何身份是当前世代的全局primitive，禁止跨帧重用。
module primary_cache;
import params;
import mod_scene;
import pt.scene;
[[vk::binding(10, 0)]]
public RWStructuredBuffer<float4> primary_ray;
[[vk::binding(26, 0)]]
public RWStructuredBuffer<uint4> primary_identity;
public bool realtime_pt()
{
    return (parameters[0].output_size.z & 32u) != 0;
}
public uint primary_pixel()
{
    return DispatchRaysIndex().y * parameters[0].image.x + DispatchRaysIndex().x;
}
