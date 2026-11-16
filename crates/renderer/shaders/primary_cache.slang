/// 本帧首命中32B；只在实时普通PT中使用。几何身份是当前世代的全局primitive，禁止跨帧重用。
module primary_cache;
import params;
import mod_scene;
import pt.scene;
[[vk::binding(10, 0)]]
public var primary_ray : RWStructuredBuffer<float4>;
[[vk::binding(26, 0)]]
public var primary_identity : RWStructuredBuffer<uint4>;
public func realtime_pt()->bool
{
    return (parameters[0].output_size.z & 32u) != 0;
}
public func primary_pixel()->uint
{
    return DispatchRaysIndex().y * parameters[0].image.x + DispatchRaysIndex().x;
}
