/// RR 主表面和反射导引；折射终点由前帧实际光路校正。
module mod_guides;
import mod_scene;
import params;
import mod_optical_history;
import pt.integrator;
import pt.reconstruction;
import pt.key_guides;
import pt.guide_output;
import pt.hit_transport;
import camera.pinhole;

[[vk::binding(19, 0)]]
[[vk::image_format("r32f")]]
var guide_depth : RWTexture2D<float>;
[[vk::binding(20, 0)]]
[[vk::image_format("rg32f")]]
var guide_motion : RWTexture2D<float2>;
[[vk::binding(21, 0)]]
[[vk::image_format("rgba16f")]]
var guide_normal : RWTexture2D<float4>;
[[vk::binding(22, 0)]]
[[vk::image_format("rgba16f")]]
var guide_diffuse : RWTexture2D<float4>;
[[vk::binding(23, 0)]]
[[vk::image_format("rgba16f")]]
var guide_specular : RWTexture2D<float4>;
[[vk::binding(24, 0)]]
[[vk::image_format("r32f")]]
var guide_distance : RWTexture2D<float>;
[[vk::binding(46, 0)]]
[[vk::image_format("rg32f")]]
var guide_specular_motion : RWTexture2D<float2>;
[[vk::binding(45, 0)]]
[[vk::image_format("r8")]]
var disocclusion : RWTexture2D<float>;

func match_rr_motion<let reflected : bool>(record: uint2, film_pixel: float2, inout motion: float2)
    ->bool
{
    let p = parameters[0];
    let previous_jitter = float2(p.previous_right.w, p.previous_up.w);
    let previous_sample_origin = 0.5 + previous_jitter;
    let seed = film_pixel + motion - previous_sample_origin;
    var match_pixel : float2;
    if (!match_optics<reflected>(record, seed, match_pixel))
        return false;
    // 只在访问旧采样格时减去旧 jitter。SDK 接收未加两帧 jitter 差的像素位移。
    motion = match_pixel + previous_sample_origin - film_pixel;
    return true;
}

struct RrOptics : pt::IGuideHistory
{
    static func boundary(key: uint, id: uint4, front: bool, reflection: bool, eta: float)->uint
    {
        return OpticalKey.boundary(key, id, front, reflection, eta);
    }
    static func apply(primary: pt::GuideEndpoint, reflected: pt::GuideEndpoint,
                      frame: pt::GuideFrame, pixel: uint2, film: float2, inout motion: float2,
                      inout specular_motion: float2)
        ->void
    {
        if (parameters[0].reconstruction.w != 2)
            return;
        let inside = optical_record(primary);
        let outside = optical_record(reflected);
        publish_optics(pixel, uint4(inside, outside));
        var reject = frame.history && ((primary.flags | reflected.flags) &
                                       (pt::GUIDE_INVALID | pt::GUIDE_SURFACE_PROXY)) != 0;
        let film_pixel = film * float2(frame.extent);
        if (frame.history)
        {
            reject = reject || (inside.x == 0 && (primary.flags & pt::GUIDE_UNPROJECTABLE) != 0) ||
                     (outside.x == 0 && (reflected.flags & pt::GUIDE_UNPROJECTABLE) != 0);
            reject = reject || any(uncovered_paths(uint2(inside.x, outside.x), film_pixel + motion,
                                                   film_pixel + specular_motion));
            if (inside.x != 0 && !match_rr_motion<false>(inside, film_pixel, motion))
                reject = true;
            if (outside.x != 0 && !match_rr_motion<true>(outside, film_pixel, specular_motion))
                reject = true;
        }
        disocclusion[pixel] = float(reject);
    }
};

public struct GuideTrace : pt::IIdentifiedGuideTrace
{

    public func intersect_identified(origin: float3, direction: float3, incident: float4,
                                     out value: pt::KeyGuideVertex, out identity: uint4)
        ->bool
    {
        value = {};
        identity = 0;
        let ray : RayDesc = { origin, 0, direction, pt::FAR };
        var unused : uint = 0;
        var found : HitObject =
                        HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
        if (!found.IsHit())
            return false;
        let primitive : uint = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
        let attr = found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        identity = uint4(primitive, asuint(attr.barycentrics), 0);
#ifdef PRIMEPT_SEPARATE_GUIDES
        var program : uint = guide_slot(primitive);
#else
        let program : uint = guide_slot(primitive) + parameters[0].programs.z;
#endif
        found.SetShaderTableIndex(program + 1u);
        var call : pt::KeyGuideCall = {};
        call.value.medium = incident;
#ifdef PRIMEPT_SER
        ReorderThread(found);
#endif
        HitObject::Invoke(found, call);
        value = call.value;
        return true;
    }
};

public struct Guides : pt::IPrefixGuides, pt::IHitGuides
{
    public var coordinate : uint2;
    public var film : float2;
    public var current : camera::Camera;
    public var previous : camera::Camera;

    func images()->pt::GuideImages
    {
        return { guide_depth,    guide_motion,   guide_normal,         guide_diffuse,
                 guide_specular, guide_distance, guide_specular_motion };
    }
    func frame()->pt::GuideFrame
    {
        let p : Params = parameters[0];
        return { current, previous, p.image.xy, p.options.y, p.reconstruction.z > 0 };
    }

    public func write_key<Trace : pt::IIdentifiedGuideTrace>(provider: Trace,
                                                             first: pt::KeyGuideVertex,
                                                             identity: uint4, origin: float3,
                                                             direction: float3, incident: float4,
                                                             delta_only: bool = false)
        ->void
    {
        pt::write_tracked_key_guides<Trace, RrOptics>(provider, images(), frame(), coordinate, film,
                                                      first, identity, origin, direction, incident,
                                                      delta_only);
    }

    public func surface_hit(found: HitObject, data: pt::TransportCall)->void
    {
        let primitive : uint = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
        let attr = found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        let identity = uint4(primitive, asuint(attr.barycentrics), 0);
        let program : uint = guide_slot(primitive) + parameters[0].programs.z;
        found.SetShaderTableIndex(program + 1u);
        var call : pt::KeyGuideCall = {};
        call.value.medium = data.path.medium;
#ifdef PRIMEPT_SER
        ReorderThread(found);
#endif
        HitObject::Invoke(found, call);
        var provider : GuideTrace;
        write_key(provider, call.value, identity, data.path.origin, data.path.direction,
                  data.path.medium);
    }

    // mode0参考与未启用EXT的兼容PT保留旧导引；HG/PT不经此通用翻译路径。
    public func surface(data: pt::TransportCall)->void
    {
        pt::write_surface_guides<ModScene, RrOptics>(ModScene(), images(), frame(), coordinate,
                                                     film, data);
    }

    public func environment(data: pt::TransportCall)->void
    {
        sky(data.path.direction);
    }

    public func sky(direction: float3)->void
    {
        let p = frame();
        let sky = pt::write_sky_guide(images(), p, coordinate, film, direction, 0);
        if (parameters[0].reconstruction.w == 2)
        {
            publish_optics(coordinate, 0);
            disocclusion[coordinate] = float(
                p.history && ((sky.flags & pt::GUIDE_UNPROJECTABLE) != 0 ||
                              any(previous_optics(film * float2(p.extent) + sky.motion) != 0)));
        }
    }
};
