/// PT叶子输入用完后复用位置和状态保存返回值；预算1..64保证depth/events各占7bit。
module mod_call;
import pt.scene;
import geometry.ray;
import bsdf.openpbr.flow;

public struct ModTransportCall
{
    public var surface_point : geometry::SurfacePoint;
    public var identity : uint4;
    public var state : uint;
    public var direction : float3;
    public var throughput : float3;
    public var radiance : float3;
    public var medium : float4;
    public var previous : float3;
    public var previous_pdf : float;
    public var eta_scale : float;
    public var pixel : uint2;
    public var sample : uint;

    public static func input(data: pt::TransportCall)->ModTransportCall
    {
        var call : ModTransportCall;
        call.surface_point = data.hit.surface;
        call.identity = data.hit.identity;
        call.state =
            uint(data.hit.front) | (data.path.depth << 1) | (data.path.dielectric_events << 8);
        call.direction = data.path.direction;
        call.throughput = data.path.throughput;
        call.radiance = data.path.radiance;
        call.medium = data.path.medium;
        call.previous = data.path.previous;
        call.previous_pdf = data.path.previous_pdf;
        call.eta_scale = data.path.eta_scale;
        call.pixel = data.pixel;
        call.sample = data.sample;
        return call;
    }

    public func unpack()->pt::TransportCall
    {
        var data : pt::TransportCall = {};
        data.hit.surface = surface_point;
        data.hit.identity = identity;
        data.hit.front = (state & 1u) != 0;
        data.path = result();
        data.path.origin = 0;
        data.path.status = pt::PATH_ACTIVE;
        data.path.depth = (state >> 1) & 127u;
        data.path.dielectric_events = (state >> 8) & 127u;
        data.pixel = pixel;
        data.sample = sample;
        return data;
    }

    public func result()->pt::PathState
    {
        var path : pt::PathState;
        path.origin = surface_point.position;
        path.direction = direction;
        path.throughput = throughput;
        path.radiance = radiance;
        path.medium = medium;
        path.previous = previous;
        path.previous_pdf = previous_pdf;
        path.eta_scale = eta_scale;
        path.depth = (state >> 2) & 127u;
        path.dielectric_events = (state >> 9) & 127u;
        path.status = state & 3u;
        return path;
    }

    [mutating]
    public func store<let flow : uint>(path: pt::PathState)->void
    {
        surface_point.position = path.origin;
        state = path.status | (path.depth << 2) | (path.dielectric_events << 9);
        direction = path.direction;
        throughput = path.throughput;
        if (openpbr::flow_continuous<flow>() || (flow & openpbr::FLOW_EMISSION) != 0)
            radiance = path.radiance;
        // 不变字段留在交接内存中，避免叶子为回写而读取并保活它们。
        if ((flow & openpbr::FLOW_SOLID_TRANSMISSION) != 0)
        {
            medium = path.medium;
            eta_scale = path.eta_scale;
        }
        previous = path.previous;
        previous_pdf = path.previous_pdf;
    }
};
