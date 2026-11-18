/// 实时 PT 的真实表面/反射代理导引；不维护独立历史。
module mod_guides;
import mod_scene;
import params;
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
[[vk::image_format("rg16f")]]
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
[[vk::binding(25, 0)]]
[[vk::image_format("rg16f")]]
var guide_specular_motion : RWTexture2D<float2>;

public struct GuideTrace : pt::IKeyGuideTrace
{

    public func intersect_guide(origin: float3, direction: float3, incident: float4,
                                out value: pt::KeyGuideVertex)
        ->bool
    {
        value = {};
        let ray : RayDesc = { origin, 0, direction, pt::FAR };
        var unused : uint = 0;
        var found : HitObject =
                        HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
        if (!found.IsHit())
            return false;
        let primitive : uint = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
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

    public func write_key<Trace : pt::IKeyGuideTrace>(provider: Trace, first: pt::KeyGuideVertex,
                                                      origin: float3, direction: float3,
                                                      incident: float4, delta_only: bool = false)
        ->void
    {
        pt::write_key_guides(provider, images(), frame(), coordinate, film, first, origin,
                             direction, incident, delta_only);
    }

    public func surface_hit(found: HitObject, data: pt::TransportCall)->void
    {
        let primitive : uint = region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex());
        let program : uint = guide_slot(primitive) + parameters[0].programs.z;
        found.SetShaderTableIndex(program + 1u);
        var call : pt::KeyGuideCall = {};
        call.value.medium = data.path.medium;
#ifdef PRIMEPT_SER
        ReorderThread(found);
#endif
        HitObject::Invoke(found, call);
        var provider : GuideTrace;
        write_key(provider, call.value, data.path.origin, data.path.direction, data.path.medium);
    }

    // mode0参考与未启用EXT的兼容PT保留旧导引；HG/PT不经此通用翻译路径。
    public func surface(data: pt::TransportCall)->void
    {
        pt::write_surface_guides(ModScene(), images(), frame(), coordinate, film, data);
    }

    public func environment(data: pt::TransportCall)->void
    {
        sky(data.path.direction);
    }

    public func sky(direction: float3)->void
    {
        pt::write_sky_guide(images(), frame(), coordinate, film, direction, 0);
    }
};
