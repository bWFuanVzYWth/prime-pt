/// PT叶子输入用完后复用位置和状态保存返回值；预算1..64保证depth/events各占7bit。
module mod_call;
import pt.scene;
import geometry.ray;
import bsdf.openpbr.flow;

public struct ModTransportCall
{
    public geometry::SurfacePoint point;
    public uint4 identity;
    public uint state;
    public float3 direction;
    public float3 throughput;
    public float3 radiance;
    public float4 medium;
    public float3 previous;
    public float previous_pdf;
    public float eta_scale;
    public uint2 pixel;
    public uint sample;

    public static ModTransportCall input(pt::TransportCall data)
    {
        ModTransportCall call;
        call.point = data.hit.surface;
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

    public pt::TransportCall unpack()
    {
        pt::TransportCall data = {};
        data.hit.surface = point;
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

    public pt::PathState result()
    {
        pt::PathState path;
        path.origin = point.position;
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
    public void store<let Flow : uint>(pt::PathState path)
    {
        point.position = path.origin;
        state = path.status | (path.depth << 2) | (path.dielectric_events << 9);
        direction = path.direction;
        throughput = path.throughput;
        if (openpbr::flow_continuous<Flow>() || (Flow & openpbr::FLOW_EMISSION) != 0)
            radiance = path.radiance;
        // 不变字段留在交接内存中，避免叶子为回写而读取并保活它们。
        if ((Flow & openpbr::FLOW_SOLID_TRANSMISSION) != 0)
        {
            medium = path.medium;
            eta_scale = path.eta_scale;
        }
        previous = path.previous;
        previous_pdf = path.previous_pdf;
    }
};
