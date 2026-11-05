/// PT叶子输入用完后复用位置和状态保存返回值；预算1..64保证depth/events各占7bit。
module mod_call;
import pt.scene;
import geometry.ray;

public struct ModTransportCall
{
    public geometry::SurfacePoint point;
    public float4 texel;
    public uint4 identity;
    public uint state;
    public float distance;
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
        call.texel = data.hit.texel;
        call.identity = data.hit.identity;
        call.state =
            uint(data.hit.front) | (data.path.depth << 1) | (data.path.dielectric_events << 8);
        call.distance = data.hit.distance;
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
        data.hit.texel = texel;
        data.hit.identity = identity;
        data.hit.front = (state & 1u) != 0;
        data.hit.distance = distance;
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
    public void store(pt::PathState path)
    {
        point.position = path.origin;
        state = path.status | (path.depth << 2) | (path.dielectric_events << 9);
        direction = path.direction;
        throughput = path.throughput;
        radiance = path.radiance;
        medium = path.medium;
        previous = path.previous;
        previous_pdf = path.previous_pdf;
        eta_scale = path.eta_scale;
    }
};
