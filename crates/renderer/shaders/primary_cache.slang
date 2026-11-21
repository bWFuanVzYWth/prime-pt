/// 本帧首命中16B：图元、两个FP32重心和FP32距离；只在实时普通PT中使用。几何身份是当前世代的全局primitive，禁止跨帧重用。
module primary_cache;
import params;
import mod_scene;
import pt.scene;
import camera.pinhole;
import pt.path_sampler;
[[vk::binding(26, 0)]]
public var primary_hit : RWStructuredBuffer<uint4>;
public func primary_direction<let frame_film : bool>(p: Params, coordinate: uint2)->float3
{
    let sampler = pt::path_sampler(coordinate, p.rr_sample, p.options.z, p.options.x);
    let offset = frame_film ? 0.5 + p.reconstruction.xy : sampler.film();
    let film = camera::camera_sample_uv_offset(coordinate, p.inverse_extent, offset);
    return camera::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
}
public func primary_direction(p: Params, coordinate: uint2)->float3
{
    return p.reconstruction.w > 0 ? primary_direction<true>(p, coordinate)
                                  : primary_direction<false>(p, coordinate);
}
public func realtime_pt()->bool
{
    return (parameters[0].output_size.z & 32u) != 0;
}
public func primary_pixel()->uint
{
    return DispatchRaysIndex().y * parameters[0].image.x + DispatchRaysIndex().x;
}
