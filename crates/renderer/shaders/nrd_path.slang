module nrd_path;
import denoise.nrd_signal;
import nrd_guides;
import mod_scene;
import pt.material_vertex;
import pt.surface_guides;
import pt.scene;
import params;
import transport.surface_material;
import openpbr.flow;

[[vk::binding(40, 0)]]
public var signal_lanes : RWStructuredBuffer<SignalLane>;

/// 用相同闭包的有色与黑色介质求值之差分离底层响应。对金属混合保留原有覆盖权重。
public func diffuse_fraction<let flow : uint>(mat: transport::SurfaceMaterial, normal: float3,
                                              view: float3, direction: float3, incident: float4,
                                              discrete: bool)
    ->float3
{
    if ((flow & (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE)) == 0 || discrete ||
        mat.pbr.surface.y >= 1 || !any(direction))
        return 0;
    if ((flow & (openpbr::FLOW_DIELECTRIC_REFLECTION | openpbr::FLOW_CONDUCTOR_REFLECTION)) == 0)
        return 1;
    let energy = ModScene().energy();
    let total = openpbr::evaluate_flow<flow>(energy, mat.pbr, normal, view, direction, incident,
                                             mat.transmitted)
                    .response;
    var dielectric = mat.pbr;
    dielectric.surface.y = 0;
    var with_base = total;
    if (mat.pbr.surface.y > 0)
        with_base = openpbr::evaluate_flow<flow>(energy, dielectric, normal, view, direction,
                                                 incident, mat.transmitted)
                        .response;
    dielectric.base_roughness.xyz = 0;
    let coat = openpbr::evaluate_flow<flow>(energy, dielectric, normal, view, direction, incident,
                                            mat.transmitted)
                   .response;
    return saturate((with_base - coat) * (1 - mat.pbr.surface.y) / max(total, 1e-20));
}

func combine(inout uint2 signal, inout uint2 moment, indirect: float3, direction: float3,
             distance: float, sampled: bool)
    ->void
{
    let direct = unpack_signal(signal);
    let first = brightness(direct.xyz);
    let second = brightness(indirect);
    let sum = first + second;
    let mean = sum > 0 ? (first * unpack_signal(moment).xyz + second * direction) / sum : direction;
    // 零辐亮度的有效路径仍提供命中距离，缺采样才写 0。
    let hit = sum > 0 ? (first * direct.w + second * min(distance, 65504)) / sum
                      : (sampled ? min(distance, 65504) : 0);
    signal = pack_signal(float4(direct.xyz + indirect, hit));
    moment = pack_signal(float4(mean, unpack_signal(moment).w));
}

public func begin_signal<let flow : uint>(index: uint, observed: pt::PrimaryLighting, hit: pt::Hit,
                                          mat: transport::SurfaceMaterial, incident: float4,
                                          view: float3, inout previous_pdf: float)
    ->float
{
    let normal = pt::shading_normal(hit, mat, view);
    let glass = (mat.pbr.control.x & 1u) != 0;
    if ((index & 1u) == 0 && !fixed_guide_candidate(flow))
        physical_guides(
            hit, mat, incident,
            pt::denoise_surface_flow<flow>(ModScene().energy(), mat, normal, view, incident));
    let side = dot(view, hit.surface.normal);
    let local_diffuse = glass ? float3(side * dot(observed.local_light.xyz, hit.surface.normal) < 0)
                              : diffuse_fraction<flow>(mat, normal, view, observed.local_light.xyz,
                                                       incident, false);
    let blocker = any(observed.sun > 0) ? ModScene().blocker_distance<false>(observed.sun_ray) : 0;
    var lane : SignalLane = {};
    lane.diffuse = pack_signal(float4(observed.local * local_diffuse, observed.local_light.w));
    lane.specular =
        pack_signal(float4(observed.local * (1 - local_diffuse), observed.local_light.w));
    lane.diffuse_direction = pack_signal(
        float4(observed.local_light.xyz, abs(dot(observed.sun_ray.direction, hit.surface.normal))));
    lane.specular_direction = pack_signal(float4(observed.local_light.xyz, float(observed.events)));
    if ((observed.events & openpbr::DISCRETE) != 0)
        previous_pdf =
            emission_prefix(DispatchRaysIndex().xy, (observed.events & openpbr::REFLECTION) != 0);
    if (glass || (fixed_guide_candidate(flow) &&
                  openpbr::is_discrete_only_flow<flow>(mat.pbr, incident, mat.transmitted)))
    {
        // 局部光和太阳的直接估计按各自的几何半球归属。采样器已计入首界面的权重。
        if (blocker == pt::FAR)
        {
            if (side * dot(observed.sun_ray.direction, hit.surface.normal) < 0)
                combine(lane.diffuse, lane.diffuse_direction, observed.sun,
                        observed.sun_ray.direction, pt::FAR, true);
            else
                combine(lane.specular, lane.specular_direction, observed.sun,
                        observed.sun_ray.direction, pt::FAR, true);
        }
    }
    else
        lane.sun = pack_signal(float4(observed.sun, blocker));
    signal_lanes[index] = lane;
    return blocker;
}

public func finish_signal(index: uint, tail: float3, direction: float3, distance: float)->void
{
    var lane = signal_lanes[index];
    let events = uint(unpack_signal(lane.specular_direction).w);
    let interior = (events & (openpbr::DIFFUSE | openpbr::TRANSMISSION)) != 0;
    let reflection = events != 0 && !interior;
    combine(lane.diffuse, lane.diffuse_direction, interior ? tail : float3(0), direction, distance,
            interior);
    combine(lane.specular, lane.specular_direction, reflection ? tail : float3(0), direction,
            distance, reflection);
    signal_lanes[index] = lane;
}
