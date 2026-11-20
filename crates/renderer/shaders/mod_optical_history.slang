/// 两路固定光路的跨帧身份与终点，按成功提交的帧奇偶交换。
module mod_optical_history;
import pt.optical_history;
import pt.reconstruction;
import mod_scene;
import params;

[[vk::binding(43, 0)]]
var optical_even : RWStructuredBuffer<uint4>;
[[vk::binding(44, 0)]]
var optical_odd : RWStructuredBuffer<uint4>;

public struct OpticalKey : pt::IOpticalPathKey
{
    public static func boundary(key: uint, hit: uint4, front: bool, reflection: bool, eta: float)
        ->uint
    {
        return pt::optical_key(key, stable_triangle(hit.x).xy,
                               asuint(eta) ^ uint(front) ^ (uint(reflection) << 1));
    }
};

public func optical_record(end: pt::GuideEndpoint)->uint2
{
    if (!end.sampled_motion || (end.flags & (pt::GUIDE_SURFACE_PROXY | pt::GUIDE_INVALID)) != 0)
        return 0;
    if ((end.flags & pt::GUIDE_SKY) != 0)
        return pt::optical_sky(end.optical_key, end.direction);
    let id = end.hit.identity;
    let b = asfloat(id.yz);
    // ABC 和 ACD 的重心坐标映入同一 quad，连续性可以跨越内部对角线。
    let uv = (id.x & 1u) == 0 ? float2(b.x + b.y, b.y) : float2(b.x, b.x + b.y);
    return pt::optical_sample(
        pt::optical_key(end.optical_key, stable_triangle(id.x).xy, uint(end.hit.front) + 2u), uv);
}

public func publish_optics(pixel: uint2, record: uint4)->void
{
    let p = parameters[0];
    let index = pixel.y * p.image.x + pixel.x;
    if ((p.output_size.w & 1u) == 0)
        optical_even[index] = record;
    else
        optical_odd[index] = record;
}

/// 已显露的直达表面可能与旧折射像深度相同。检查旧投影最近的实际采样属于哪条光路。
public func previous_optics(film_pixel: float2)->uint2
{
    let p = parameters[0];
    let raster = film_pixel - float2(p.previous_right.w, p.previous_up.w);
    if (!all(raster >= 0) || !all(raster < float2(p.image.xy)))
        return 0;
    let xy = uint2(raster);
    let index = xy.y * p.image.x + xy.x;
    let pair = (p.output_size.w & 1u) == 0 ? optical_odd[index] : optical_even[index];
    return pair.xz;
}

public func uncovered_paths(keys: uint2, previous_primary: float2, previous_reflected: float2)
    ->bool2
{
    var previous : uint2 = 0;
    if (keys.x == 0)
        previous = previous_optics(previous_primary);
    if (keys.y != 0)
        previous.y = 0;
    else if (keys.x != 0 || any(previous_primary != previous_reflected))
        previous.y = previous_optics(previous_reflected).y;
    return previous != 0;
}

public func match_optics<let reflection : bool>(record: uint2, seed: float2, out matched: float2)
    ->bool
{
    let p = parameters[0];
    if ((p.output_size.w & 1u) == 0)
        return pt::previous_optical_pixel<reflection>(optical_odd, p.image.xy, record, seed,
                                                      matched);
    return pt::previous_optical_pixel<reflection>(optical_even, p.image.xy, record, seed, matched);
}
