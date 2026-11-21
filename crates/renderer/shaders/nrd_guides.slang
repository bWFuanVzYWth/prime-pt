/// NRD 的固定几何导引与 DLSS RR 共享折射优先的虚拟主表面。
module nrd_guides;
import mod_scene;
import denoise.nrd_signal;
import pt.surface_guides;
import transport.medium;
import mod_materials;
import pt.reconstruction;
import mod_optical_history;
import pt.scene;
import transport.surface_material;
import openpbr.flow;
import camera.pinhole;
import denoise.guide;
import params;

[[vk::binding(15, 0)]]
public var prefix_image : RWStructuredBuffer<float4>;
[[vk::binding(19, 0)]]
[[vk::image_format("r32f")]]
var depth_image : RWTexture2D<float>;
[[vk::binding(41, 0)]]
[[vk::image_format("rgba32f")]]
var motion_image : RWTexture2D<float4>;
[[vk::binding(21, 0)]]
[[vk::image_format("rgba16f")]]
var normal_image : RWTexture2D<float4>;
[[vk::binding(22, 0)]]
[[vk::image_format("rgba16f")]]
var albedo_image : RWTexture2D<float4>;
[[vk::binding(23, 0)]]
[[vk::image_format("rgba16f")]]
var specular_color_image : RWTexture2D<float4>;

[[vk::binding(24, 0)]]
[[vk::image_format("r32f")]]
var reflected_depth : RWTexture2D<float>;
[[vk::binding(25, 0)]]
[[vk::image_format("rg16f")]]
var reflected_normal : RWTexture2D<float2>;
[[vk::binding(42, 0)]]
[[vk::image_format("rgba32f")]]
var reflected_motion : RWTexture2D<float4>;
public func sky_signal(pixel: uint2)->void
{
    prefix_image[pixel.y * parameters[0].image.x + pixel.x].xyz = 0;
    depth_image[pixel] = reflected_depth[pixel] = 1e7;
    motion_image[pixel] = reflected_motion[pixel] = 0;
    normal_image[pixel] = float4(0, 0, 1, 1);
    reflected_normal[pixel] = 0;
    albedo_image[pixel] = specular_color_image[pixel] = 0;
    publish_optics(pixel, 0);
}
public func fixed_guide_candidate(flow: uint)->bool
{
    return (flow & (openpbr::FLOW_DELTA_REFLECTION | openpbr::FLOW_DELTA_TRANSMISSION)) != 0;
}

struct SurfaceGuide
{
    var depth : float;
    var motion : float3;
    var normal : float3;
    var roughness : float;
    var factor : float3;
    var use_sh_color : bool;
    var replaced : bool;
    var material : uint;
    var optical : uint2;
    var emission : float3;
    var emission_vertices : uint;
};

func material_class(hit: pt::Hit, mat: transport::SurfaceMaterial)->uint
{
    let flags = material_words.Load(material_id(hit.identity.x) * 112u + 72u);
    return (mat.pbr.control.x & 1u) != 0 ? 3u
           : (flags & 8u) != 0           ? 2u
           : mat.pbr.surface.y >= 0.5    ? 1u
                                         : 0u;
}

func geometry_guide(position: float3, normal: float3, previous_target: float3,
                    previous_depth: float, sky: bool, valid: bool)
    ->SurfaceGuide
{
    let p = parameters[0];
    let previous : camera::Camera = { p.previous_position, p.previous_forward, p.previous_right,
                                      p.previous_up };
    let film = camera::camera_sample_uv_offset(DispatchRaysIndex().xy, p.inverse_extent,
                                               0.5 + p.reconstruction.xy);
    var g : SurfaceGuide = {};
    g.depth = dot(position - p.origin.xyz, p.current_camera.forward.xyz);
    g.normal = dot(normal, p.origin.xyz - position) < 0 ? -normal : normal;
    if (p.reconstruction.z > 0)
    {
        var mv : float2;
        let projected =
            denoise::denoise_motion(previous, film, previous_target, sky, p.image.xy, mv);
        let dz = previous_depth - g.depth;
        g.motion = valid && projected && all(abs(mv) < 65504) && abs(dz) < 65504
                       ? float3(mv, dz)
                       : float3(float2(p.image.xy), 0);
    }
    return g;
}
func write_guides(interior: SurfaceGuide, reflection: SurfaceGuide)->void
{
    let pixel = DispatchRaysIndex().xy;
    var interior = interior;
    var reflection = reflection;
    let p = parameters[0];
    if (p.reconstruction.z > 0)
    {
        let film_pixel = float2(pixel) + 0.5 + p.reconstruction.xy;
        let uncovered =
            uncovered_paths(uint2(interior.optical.x, reflection.optical.x),
                            film_pixel + interior.motion.xy, film_pixel + reflection.motion.xy);
        if (uncovered.x)
            interior.motion = float3(float2(p.image.xy), 0);
        if (uncovered.y)
            reflection.motion = float3(float2(p.image.xy), 0);
    }
    prefix_image[pixel.y * parameters[0].image.x + pixel.x].xyz =
        interior.emission + reflection.emission;
    publish_optics(pixel, uint4(interior.optical, reflection.optical));
    depth_image[pixel] = interior.depth;
    normal_image[pixel] = float4(interior.normal, interior.roughness);
    motion_image[pixel] =
        float4(interior.motion, float(interior.material + (interior.replaced ? 8u : 0u) +
                                      (interior.emission_vertices << 4)));
    albedo_image[pixel] = float4(interior.factor, interior.use_sh_color ? 2 : 1);
    reflected_depth[pixel] = reflection.depth;
    reflected_normal[pixel] = oct_normal(reflection.normal);
    reflected_motion[pixel] =
        float4(reflection.motion,
               float(reflection.material + (reflection.use_sh_color ? 4u : 0u) +
                     (reflection.replaced ? 8u : 0u) + (reflection.emission_vertices << 4)));
    specular_color_image[pixel] = float4(reflection.factor, reflection.roughness);
}
/// 首个离散事件已经选定采样分量，剩余的固定顶点只扣除自身发光。
public func emission_prefix(pixel: uint2, reflection: bool)->float
{
    let flags = reflection ? reflected_motion[pixel].w : motion_image[pixel].w;
    let vertices = uint(flags) >> 4;
    return -float(max(1u, vertices) - 1u);
}
/// 两路都落在当前非 delta 表面时只投影一次，分别保存方向能量。
public func physical_guides(hit: pt::Hit, mat: transport::SurfaceMaterial, incident: float4,
                            energies: denoise::DenoiseGuides)
    ->void
{
    let p = parameters[0];
    let pos = hit.surface.position;
    let pose = ModScene().guide_pose(hit);
    var inside = geometry_guide(
        pos, energies.normal_roughness.xyz, pose.previous_position,
        dot(pose.previous_position - p.previous_position.xyz, p.previous_forward.xyz), false,
        pose.known);
    inside.roughness = mat.pbr.base_roughness.w;
    inside.use_sh_color = (mat.pbr.control.x & 1u) != 0;
    inside.material = material_class(hit, mat);
    let beer = transport::segment_transmittance(incident.yzw, hit.distance);
    var outside = inside;
    inside.factor = beer * energies.diffuse_albedo;
    inside.emission = beer * mat.emission;
    outside.factor = beer * energies.specular_albedo;
    write_guides(inside, outside);
}

func lobe_surface<let reflection : bool, Scene : pt::IGuideScene>(scene: Scene,
                                                                  first: pt::TransportCall,
                                                                  front: transport::SurfaceMaterial)
    ->SurfaceGuide
{
    let p = parameters[0];
    let end = pt::trace_lobe_guide<reflection, OpticalKey>(scene, first, p.options.y, front,
                                                           p.previous_position.xyz, p.options.w);
    let sky = (end.flags & pt::GUIDE_SKY) != 0;
    let replaced = end.psr.count != 0;
    let glass = (front.pbr.control.x & 1u) != 0;
    var position = first.hit.surface.position;
    var normal = pt::shading_normal(first.hit, front, -first.path.direction);
    var pose_delta : float3 = 0;
    if (!sky)
    {
        pt::psr_surface(end.psr, end.camera_origin, end.camera_direction, end.hit.surface.position,
                        end.hit.surface.normal,
                        pt::shading_normal(end.hit, end.material, -end.direction), position,
                        normal);
        if (end.pose.known)
            pose_delta = end.pose.previous_position - end.hit.surface.position;
    }
    var target = sky ? end.camera_direction : position + pt::psr_direction(end.psr, pose_delta);
    var previous_z = dot(position - p.previous_position.xyz, p.previous_forward.xyz);
    var valid =
        (end.flags & (pt::GUIDE_SURFACE_PROXY | pt::GUIDE_INVALID)) == 0 && (sky || end.pose.known);
    let record = optical_record(end);
    if ((end.sampled_motion || (replaced && end.optical_motion.moving)) && p.reconstruction.z > 0)
    {
        var previous_ray : float3;
        let solved = end.optical_motion.previous_ray(
            end.camera_direction, sky ? float3(0) : pose_delta,
            sky ? end.direction : end.hit.surface.normal, sky, previous_ray);
        if (valid && record.x != 0)
        {
            let previous : camera::Camera = { p.previous_position, p.previous_forward,
                                              p.previous_right, p.previous_up };
            let projection = camera::camera_project_direction(previous, previous_ray, p.image.xy);
            let seed = projection.pixel - 0.5;
            var matched : float2;
            valid = solved && projection.valid && match_optics<reflection>(record, seed, matched);
            if (!valid)
            {
                // 大幅横移时一阶光线微分可能越过旧视野，从虚拟平面再尝试同一个实测终点。
                let plane = sky ? camera::camera_project_direction(previous, target, p.image.xy)
                                : camera::camera_project(previous, target, p.image.xy);
                if (plane.valid && (!solved || !projection.valid ||
                                    any(abs(plane.pixel - projection.pixel) > 0.5)))
                    valid = match_optics<reflection>(record, plane.pixel - 0.5, matched);
            }
            if (valid && (!solved || !projection.valid || any(abs(matched - seed) > 0.045)))
                previous_ray =
                    camera::camera_direction(previous, (matched + 0.5) * p.inverse_extent);
        }
        else
            valid = valid && solved;
        if (sky)
            target = previous_ray;
        else
        {
            let n = pt::psr_direction(end.psr, end.hit.surface.normal);
            let denom = dot(n, previous_ray);
            var plane = pt::psr_point(end.psr, end.hit.surface.position + pose_delta);
            if (end.psr.count == 1 && end.psr.odd && end.sampled_motion)
            {
                // 贴图镜面的反射法线和几何法线不同。旧射线落在界面的另一点，
                // 需以该点展开旧终面，才能给 NRD 正确的 previous view-Z。
                let interface_normal = first.hit.surface.normal;
                let optical_normal = pt::shading_normal(first.hit, front, -first.path.direction);
                let pose = scene.guide_pose(first.hit);
                let previous_point =
                    pose.known ? pose.previous_position : first.hit.surface.position;
                let incidence = dot(interface_normal, previous_ray);
                let distance =
                    abs(incidence) > 1e-6
                        ? dot(interface_normal, previous_point - p.previous_position.xyz) /
                              incidence
                        : 0;
                valid = valid && distance > 0;
                let offset =
                    p.previous_position.xyz + distance * previous_ray - first.hit.surface.position;
                plane += 2 * optical_normal * dot(optical_normal, offset);
            }
            let t = abs(denom) > 1e-5 ? dot(plane - p.previous_position.xyz, n) / denom : 0;
            valid = valid && t > 0 && isfinite(t);
            target = p.previous_position.xyz + t * previous_ray;
        }
    }
    if (sky && p.reconstruction.z > 0)
    {
        // 天空没有平移视差，有限 viewZ 属于首个界面。沿上一帧的天空方向求同一界面深度。
        let pose = scene.guide_pose(first.hit);
        let plane = pose.known ? pose.previous_position : first.hit.surface.position;
        let n = first.hit.surface.normal;
        let denom = dot(n, target);
        let t = abs(denom) > 1e-5 ? dot(n, plane - p.previous_position.xyz) / denom : 0;
        previous_z = t * dot(target, p.previous_forward.xyz);
        valid = valid && previous_z > 0;
    }
    else if (!sky)
        previous_z = dot(target - p.previous_position.xyz, p.previous_forward.xyz);
    var g = geometry_guide(position, normal, target, previous_z, sky, valid);
    g.optical = record;
    g.emission = end.fixed_emission;
    g.emission_vertices = end.emission_vertices;
    g.replaced = replaced;
    g.roughness = reflection && glass ? front.pbr.base_roughness.w
                  : sky               ? front.pbr.base_roughness.w
                                      : end.material.pbr.base_roughness.w;
    g.material = sky ? material_class(first.hit, front) : material_class(end.hit, end.material);
    g.use_sh_color = glass || replaced ||
                     openpbr::is_discrete_only(front.pbr, first.path.medium, front.transmitted);
    var terminal = float3(1);
    if (!sky)
    {
        let e = pt::denoise_surface(scene.energy(), end.material,
                                    pt::shading_normal(end.hit, end.material, -end.direction),
                                    -end.direction, end.incident);
        // 进入虚拟面后两种终点波瓣均属于同一相机分量。
        terminal = replaced     ? e.diffuse_albedo + e.specular_albedo
                   : reflection ? e.specular_albedo
                                : e.diffuse_albedo;
    }
    g.factor = end.attenuation * terminal;
    return g;
}
public func write_fixed_guides<Scene : pt::IGuideScene>(scene: Scene, first: pt::TransportCall)
    ->void
{
    let front = scene.guide_material(first.hit, first.path.medium);
    let discrete = openpbr::is_discrete_only(front.pbr, first.path.medium, front.transmitted);
    if (!discrete && ((front.pbr.control.x & 1u) != 0 ||
                      front.pbr.base_roughness.w * front.pbr.base_roughness.w >= 1e-4))
    {
        physical_guides(
            first.hit, front, first.path.medium,
            pt::denoise_surface(scene.energy(), front,
                                pt::shading_normal(first.hit, front, -first.path.direction),
                                -first.path.direction, first.path.medium));
        return;
    }
    let inside = lobe_surface<false>(scene, first, front);
    let outside = lobe_surface<true>(scene, first, front);
    write_guides(inside, outside);
}
