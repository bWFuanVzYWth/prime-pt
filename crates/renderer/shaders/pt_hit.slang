/// 普通PT生产入口；材质来源单独适配，路径和数值叶子由引擎维护。
module pt_hit;
import pt.hit_transport;
import pt.material_vertex;
import pt.scene;
import pt.denoise;
import pt.integrator;
import pt.path_sampler;
import geometry.ray;
import bsdf.openpbr.api;
import bsdf.openpbr.flow;
import params;
import geometry.camera;
import mod_scene;
import mod_guides;
#ifndef OPENPBR_FLOW
#define OPENPBR_FLOW 131
#endif
[[vk::binding(7, 0)]]
RWStructuredBuffer<float4> output_image;
[[vk::binding(8, 0)]]
RWStructuredBuffer<float4> accumulation;
[[vk::binding(9, 0)]]
RWStructuredBuffer<uint> sample_counts;
[[vk::binding(15, 0)]]
RWStructuredBuffer<float4> prefix_image;
pt::Hit object_hit(HitObject object, float3 direction)
{
    BuiltInTriangleIntersectionAttributes attributes =
        object.GetAttributes<BuiltInTriangleIntersectionAttributes>();
    float3 vertices[3] = object.GetTriangleVertexPositions();
    // EXT返回object-space顶点，与position-fetch closest-hit采用相同误差界。
    pt::Hit hit = {};
    hit.surface = geometry::triangle_point(
        vertices[0], vertices[1], vertices[2], attributes.barycentrics,
        transpose(object.GetObjectToWorld()), transpose(object.GetWorldToObject()));
    uint primitive = region_primitive(object.GetInstanceID(), object.GetPrimitiveIndex());
    uint normal = mapped_shading_normal(
        primitive, attributes.barycentrics, vertices[1] - vertices[0], vertices[2] - vertices[0],
        (float3x3)transpose(object.GetObjectToWorld()), hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = object.GetRayDesc().TMax;
    hit.front = dot(direction, hit.surface.normal) < 0;
    return hit;
}

public struct HitScene : pt::IHitScene
{
    public pt::Hit load_hit(HitObject o, float3 d)
    {
        return object_hit(o, d);
    }
    public uint program(pt::Hit h)
    {
        return materials[material_id(h.identity.x)].control.x >> 2u;
    }
    public uint reorder_bits()
    {
        return 32u;
    }
    public pt::MaterialContext material_context(pt::Hit h)
    {
        return { h.surface.normal, h.identity, uint(h.front) };
    }
    public bool continuous(pt::Hit h)
    {
        return (h.capability &
                (openpbr::FLOW_DIFFUSE | openpbr::FLOW_SUBSURFACE |
                 openpbr::FLOW_CONTINUOUS_REFLECTION | openpbr::FLOW_CONTINUOUS_TRANSMISSION)) != 0;
    }
    public bool may_evaluate_both_sides(pt::Hit h)
    {
        // 作者法线可跨越几何半球；精确支持域由材质叶子的着色法线判断。
        return (materials[material_id(h.identity.x)].control.w & 1u) != 0 ||
               (h.capability & (openpbr::FLOW_SOLID_TRANSMISSION | openpbr::FLOW_THIN_TRANSMISSION |
                                openpbr::FLOW_SUBSURFACE)) != 0;
    }
    public bool solid(pt::Hit h)
    {
        return (h.capability & openpbr::FLOW_SOLID_TRANSMISSION) != 0;
    }

    public pt::SurfaceMaterial resolve(pt::Hit hit, uint cap, float4 incident)
    {
        return ModScene().resolve(hit, cap, incident);
    }
    public pt::SurfaceMaterial resolve_flow(pt::Hit hit, uint flow, float4 incident)
    {
        return ModScene().resolve_flow(hit, flow, incident);
    }
    public openpbr::Energy energy()
    {
        return ModScene().energy();
    }
    public pt::LightSample sample_sun(geometry::SurfacePoint p, float2 u)
    {
        return ModScene().sample_sun(p, u);
    }
    public pt::LightSample sample_light(geometry::SurfacePoint p, float3 u)
    {
        return ModScene().sample_light(p, u);
    }
    public float3 light_emission(geometry::SurfacePoint p, pt::LightSample s, bool solar)
    {
        return ModScene().light_emission(p, s, solar);
    }
    public float emission_pdf(pt::Hit hit, float3 previous)
    {
        return ModScene().emission_pdf(hit, previous);
    }
    public bool occluded(geometry::Ray ray)
    {
        return ModScene().occluded(ray);
    }
    public pt::Environment environment(float3 d, float3 p, uint depth)
    {
        return ModScene().environment(d, p, depth);
    }
    public RaytracingAccelerationStructure acceleration()
    {
        return scene;
    }
    public void resolve_hit(inout pt::Hit hit, float4 incident)
    {
        hit.capability = materials[material_id(hit.identity.x)].control.z >> 20u;
    }
};
[shader("closesthit")]
void shade_hit(inout pt::MaterialCall call, BuiltInTriangleIntersectionAttributes attributes)
{
    pt::Hit hit = {};
    hit.surface.normal = call.context.normal;
    hit.identity = call.context.attributes;
    hit.front = (call.context.state & 1u) != 0;
    pt::shade_material<OPENPBR_FLOW, ModScene>(ModScene(), call, hit, -WorldRayDirection());
}
[shader("raygeneration")]
void prefix_empty()
{
}
[shader("raygeneration")]
void render_fused()
{

    Params params = parameters[0];
    uint2 coordinate = DispatchRaysIndex().xy;
    uint pixel = coordinate.y * params.image.x + coordinate.x;
    bool diagnostic = (params.output_size.z & 1u) != 0;
    if (!diagnostic && accumulation[pixel].w < 0)
    {
        output_image[pixel] = float4(0, 0, 0, accumulation[pixel].w);
        return;
    }
    pt::TransportCall data = {};
    data.pixel = coordinate;
    data.sample =
        params.rr_sample +
        (sample_counts[pixel] >= params.image.z ? sample_counts[pixel] - params.image.z : 0u);
    pt::PathSampler sampler =
        pt::path_sampler(coordinate, data.sample, params.options.z, params.options.x);
    float2 film =
        (float2(coordinate) +
         (params.reconstruction.w > 0 ? 0.5 + params.reconstruction.xy : sampler.film())) /
        float2(params.image.xy);
    data.path.origin = params.origin.xyz;
    data.path.direction = geometry::camera_scaled_direction(params.forward.xyz, params.right.xyz,
                                                            params.up.xyz, film);
    data.path.throughput = 1;
    data.path.eta_scale = 1;
    data.path.medium = params.initial_medium;
    pt::DenoiseCamera current = geometry::camera_from_scaled(params.origin.xyz, params.forward.xyz,
                                                             params.right.xyz, params.up.xyz);
    pt::DenoiseCamera previous = { params.previous_position, params.previous_forward,
                                   params.previous_right, params.previous_up };
    Guides guides = { coordinate, film, current, previous };
    float distance;
    if (params.reconstruction.w > 0)
        distance = pt::trace_hit_path(HitScene(), guides, data, sampler, settings(params));
    else
        distance =
            pt::trace_hit_path(HitScene(), pt::NoPrefixGuides(), data, sampler, settings(params));
    prefix_image[pixel] = float4(0, 0, 0, distance);
    output_image[pixel] =
        float4(data.path.radiance, data.path.status == pt::PATH_INVALID            ? -1
                                   : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                   : 0);
}
