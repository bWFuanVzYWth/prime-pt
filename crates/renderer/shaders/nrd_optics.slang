/// NRD 与 RR 共用折射优先的固定导引；照明观察者只负责该光路的权重和互补反射。
module nrd_optics;
import nrd_path;
import mod_scene;
import mod_materials;
import params;
import geometry.ray;
import pt.scene;
import pt.integrator;
import pt.reconstruction;
import pt.path_sampler;
import openpbr.specialized;
import openpbr.flow;
import transport.medium;
import transport.surface_material;
import denoise.nrd_signal;

/// 按资源期 Flow 排除绝无纯镜面或透射事件的材质，候选纹素仍由 RR 的固定导引判定。
public func optical_candidate(flow: uint)->bool
{
    return (flow & openpbr::FLOW_DELTA_TRANSMISSION) != 0 ||
           ((flow & openpbr::FLOW_DELTA_REFLECTION) != 0 &&
            (flow & openpbr::FLOW_CONDUCTOR_REFLECTION) != 0);
}

/// 两种 SBT 共用的传输入口。接收面的入射段吸收已归入前缀，BSDF 从单位吞吐开始；
/// tail 完成整条后缀并返回其首段距离，逃逸返回 FAR。
public interface INrdOptics
{
    func intersect(origin: float3, direction: float3, out hit: pt::Hit)->bool;
    func shade_receiver(inout data: pt::TransportCall, lane: uint)->void;
    func tail(inout data: pt::TransportCall)->float;
};

struct OpticalScene<Trace : INrdOptics> : pt::IGuideScene
{
    var trace : Trace;
    func intersect(origin: float3, direction: float3, out hit: pt::Hit)->bool
    {
        return trace.intersect(origin, direction, hit);
    }
    func resolve_hit(inout hit: pt::Hit, incident: float4)->void
    {
    }
    func guide_material(hit: pt::Hit, incident: float4)->transport::SurfaceMaterial
    {
        return resolve_material<false, true>(hit, ~0u, ~0u, parameters[0].block_emission_scale);
    }
    func energy()->openpbr::Energy
    {
        return ModScene().energy();
    }
    func guide_pose(hit: pt::Hit)->pt::GuidePose
    {
        return ModScene().guide_pose(hit);
    }
};

func optical_sample(vertex: pt::GuideEndpoint, normal: float3, event_sample: float)->openpbr::Sample
{
    let m = vertex.material;
    if ((m.pbr.control.x & 1u) == 0)
        return openpbr::sample_delta_specialized<openpbr::CAP_DELTA_CONDUCTOR>(
            m.pbr, normal, -vertex.direction, event_sample, vertex.incident, m.transmitted);
    if ((m.pbr.control.x & 2u) != 0)
        return openpbr::sample_delta_specialized<openpbr::CAP_DELTA_THIN_DIELECTRIC>(
            m.pbr, normal, -vertex.direction, event_sample, vertex.incident, m.transmitted);
    return openpbr::sample_delta_specialized<openpbr::CAP_DELTA_SOLID_DIELECTRIC>(
        m.pbr, normal, -vertex.direction, event_sample, vertex.incident, m.transmitted);
}

struct OpticalPrefix : pt::IGuidePrefix
{
    var weight : float3;
    var emission : float3;
    var dielectric_events : uint;
    var sampler : pt::PathSampler;
    var alternate : pt::PathState;
    var total_mass : float;
    var selected_mass : float;
    var previous : float3;

    [mutating]
    func boundary(vertex: pt::GuideEndpoint, reflection: bool)->void
    {
        let m = vertex.material;
        let hit = vertex.hit;
        weight *= transport::segment_transmittance(vertex.incident.yzw, hit.distance);
        emission += weight * m.emission;
        let solid = (m.pbr.control.x & 3u) == 1u;
        dielectric_events += uint(solid);
        let n = pt::shading_normal(hit, m);
        let next = optical_sample(vertex, n, 0);
        previous = hit.surface.position;
        if (!reflection)
        {
            // 每条固定前缀的每个分岔只属于一个互补项。按能量选一个继续追踪，
            // 用包含概率补偿，限制多层玻璃的射线数并保留各界面的 Fresnel 反射。
            let other = optical_sample(vertex, n, asfloat(0x3f7fffffu));
            if ((other.events & openpbr::REFLECTION) != 0 &&
                pt::scatter_side_supported(hit, m, -vertex.direction, other))
            {
                let beta = weight * other.response;
                let mass = brightness(beta);
                total_mass += mass;
                if (mass > 0 &&
                    (selected_mass == 0 || sampler.roulette(vertex.psr.count) * total_mass < mass))
                {
                    selected_mass = mass;
                    alternate.origin = geometry::offset_origin(hit.surface, other.direction);
                    alternate.direction = other.direction;
                    alternate.throughput = beta;
                    alternate.medium = vertex.incident;
                    alternate.previous = previous;
                    let relative_ior = vertex.incident.x / parameters[0].initial_medium.x;
                    alternate.eta_scale = relative_ior * relative_ior;
                    alternate.depth = vertex.queries;
                    alternate.dielectric_events = dielectric_events;
                    alternate.status = pt::PATH_ACTIVE;
                }
            }
        }
        weight *=
            pt::scatter_side_supported(hit, m, -vertex.direction, next) ? next.response : float3(0);
        if (solid && !reflection)
            weight *= m.boundary_filter;
    }
};

/// 成功时每个 lane 交付完整相机样本；预算代理交回普通首面着色，不发布半条光路。
public func render_optical<Trace : INrdOptics>(trace: Trace, inout data: pt::TransportCall,
                                               lane: uint)
    ->bool
{
    let p = parameters[0];
    let pixel = data.pixel.y * p.image.x + data.pixel.x;
    let sampler = pt::path_sampler(data.pixel, data.sample, p.options.z, p.options.x);
    var prefix
        : OpticalPrefix = { float3(1), float3(0), 0u, sampler, pt::PathState(), 0, 0, float3(0) };
    let scene : OpticalScene<Trace> = { trace };
    let end = pt::trace_guide_observed(scene, data, p.options.y, false, prefix);
    if (end.psr.count == 0 || (end.flags & (pt::GUIDE_INVALID | pt::GUIDE_SURFACE_PROXY)) != 0 ||
        prefix.dielectric_events > p.options.w)
        return false;
    var extra = prefix.emission;
    if (prefix.selected_mass > 0)
    {
        var path = data;
        path.path = prefix.alternate;
        path.path.throughput *= prefix.total_mass / prefix.selected_mass;
        trace.tail(path);
        if (path.path.status != pt::PATH_DONE)
        {
            data.path = path.path;
            return true;
        }
        extra += path.path.radiance;
    }
    let earlier = data.path.radiance;
    data.path.radiance = 0;
    data.path.depth = end.psr.count;
    data.path.dielectric_events = prefix.dielectric_events;
    data.path.direction = end.direction;
    data.path.medium = end.incident;
    data.path.previous = prefix.previous;
    if ((end.flags & pt::GUIDE_SKY) != 0)
    {
        if (lane == 0)
            sky_signal(data.pixel);
        data.path.throughput = prefix.weight;
        pt::escape(ModScene(), data.path);
        data.path.radiance += earlier + extra;
        return true;
    }
    var position : float3;
    var normal : float3;
    pt::psr_surface(end.psr, end.camera_origin, end.camera_direction, end.hit.surface.position,
                    end.hit.surface.normal, pt::shading_normal(end.hit, end.material), position,
                    normal);
    let eye = -end.camera_direction;
    if (lane == 0)
        surface_guides(end.hit, end.material, position, normal, eye, end.incident, true);
    let weight =
        prefix.weight * transport::segment_transmittance(end.incident.yzw, end.hit.distance);
    data.hit = end.hit;
    data.hit.distance = 0;
    trace.shade_receiver(data, lane);
    let direct = data.path.radiance;
    data.path.radiance = 0;
    let direction = data.path.direction;
    let distance = trace.tail(data);
    finish_signal(pixel * 2 + lane, max(data.path.radiance, 0), direction, distance, eye, end.psr,
                  weight, extra);
    data.path.radiance = earlier + extra + weight * (direct + data.path.radiance);
    return true;
}
