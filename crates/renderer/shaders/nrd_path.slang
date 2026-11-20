module nrd_path;
import denoise.nrd_signal;
import mod_materials;
import mod_scene;
import primary_cache;
import pt.material_vertex;
import pt.scene;
import denoise.guide;
import camera.pinhole;
import params;
import geometry.ray;
import transport.surface_material;
import openpbr.flow;

[[vk::binding(40, 0)]]
public var signal_lanes : RWStructuredBuffer<SignalLane>;
[[vk::binding(19, 0)]]
[[vk::image_format("r32f")]]
var depth_image : RWTexture2D<float>;
[[vk::binding(41, 0)]]
[[vk::image_format("rgba16f")]]
var motion_image : RWTexture2D<float4>;
[[vk::binding(21, 0)]]
[[vk::image_format("rgba16f")]]
var normal_image : RWTexture2D<float4>;
[[vk::binding(22, 0)]]
[[vk::image_format("rgba16f")]]
var albedo_image : RWTexture2D<float4>;
[[vk::binding(23, 0)]]
[[vk::image_format("rgba16f")]]
var f0_image : RWTexture2D<float4>;

public func sky_signal(pixel: uint2)->void
{
    depth_image[pixel] = 1e7;
    motion_image[pixel] = 0;
    normal_image[pixel] = float4(0, 0, 1, 1);
    albedo_image[pixel] = 0;
    f0_image[pixel] = 0;
}
func surface_guides(hit: pt::Hit, mat: transport::SurfaceMaterial, normal: float3, view: float3,
                    incident: float4)
    ->void
{
    let p = parameters[0];
    let pixel = DispatchRaysIndex().xy;
    let previous : camera::Camera = { p.previous_position, p.previous_forward, p.previous_right,
                                      p.previous_up };
    let film = camera::camera_sample_uv_offset(pixel, p.inverse_extent, 0.5 + p.reconstruction.xy);
    var motion : float2 = 0;
    if (p.reconstruction.z > 0 &&
        !denoise::denoise_motion(previous, film, hit.surface.position, false, p.image.xy, motion))
        motion = float2(p.image.xy);
    let depth = dot(hit.surface.position - p.origin.xyz, p.current_camera.forward.xyz);
    let previous_depth =
        dot(hit.surface.position - p.previous_position.xyz, p.previous_forward.xyz);
    motion_image[pixel] = float4(motion, p.reconstruction.z > 0 ? previous_depth - depth : 0, 0);
    depth_image[pixel] = depth;
    normal_image[pixel] =
        float4(dot(normal, view) < 0 ? -normal : normal, mat.pbr.base_roughness.w);
    let metal = mat.pbr.surface.y;
    let flags = material_words.Load(material_id(hit.identity.x) * 112u + 72u);
    let material_class = mat.pbr.control.x != 0 ? 3u
                         : (flags & 8u) != 0    ? 2u
                         : metal >= 0.5         ? 1u
                                                : 0u;
    let base = mat.pbr.base_roughness.xyz;
    let f = (mat.pbr.surface.x - incident.x) / max(mat.pbr.surface.x + incident.x, 1e-6);
    albedo_image[pixel] = float4((1 - metal) * base, 1);
    f0_image[pixel] = float4(lerp(float3(f * f), base, metal), float(material_class));
}

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

public func begin_signal<let flow : uint>(index: uint, observed: pt::PrimaryLighting, hit: pt::Hit,
                                          mat: transport::SurfaceMaterial, incident: float4,
                                          view: float3, direction: float3)
    ->float
{
    let normal = pt::shading_normal(hit, mat);
    if ((index & 1u) == 0)
        surface_guides(hit, mat, normal, view, incident);
    let fraction =
        diffuse_fraction<flow>(mat, normal, view, observed.local_light.xyz, incident, false);
    let blocker =
        any(observed.sun > 0) ? ModScene().blocker_distance<false>(observed.sun_ray) : pt::FAR;
    var lane : SignalLane = {};
    lane.sun = pack_signal(float4(observed.sun, blocker));
    lane.emission = pack_signal(float4(observed.emitted, 0));
    lane.diffuse = pack_signal(float4(observed.local * fraction, observed.local_light.w));
    lane.specular = pack_signal(float4(observed.local * (1 - fraction), observed.local_light.w));
    lane.diffuse_direction = pack_signal(float4(observed.local_light.xyz, 0));
    lane.specular_direction = lane.diffuse_direction;
    lane.fraction =
        pack_signal(float4(diffuse_fraction<flow>(mat, normal, view, direction, incident,
                                                  (observed.events & openpbr::DISCRETE) != 0),
                           float(observed.events)));
    signal_lanes[index] = lane;
    return blocker;
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
    moment = pack_signal(float4(mean, 0));
}
public func finish_signal(index: uint, tail: float3, direction: float3, distance: float,
                          view: float3)
    ->void
{
    var lane = signal_lanes[index];
    let fraction = unpack_signal(lane.fraction);
    let events = uint(fraction.w);
    combine(lane.diffuse, lane.diffuse_direction, tail * fraction.xyz, direction, distance,
            (events & openpbr::DIFFUSE) != 0);
    combine(lane.specular, lane.specular_direction, tail * (1 - fraction.xyz), direction, distance,
            (events & (openpbr::GLOSSY | openpbr::DISCRETE)) != 0);
    lane.fraction = pack_signal(float4(view, 0));
    signal_lanes[index] = lane;
}
