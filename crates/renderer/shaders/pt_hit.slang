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
import mod_decode;
import mod_guides;
#ifndef OPENPBR_FLOW
#error OPENPBR_FLOW must come from a material key or an explicit raygen placeholder
#endif
#ifndef OPENPBR_VARIANT
#error OPENPBR_VARIANT must come from a material key or an explicit raygen placeholder
#endif
#ifndef PRIMEPT_LIGHT_MASK
#define PRIMEPT_LIGHT_MASK 3
#endif
#ifndef PRIMEPT_RECONSTRUCTION
#define PRIMEPT_RECONSTRUCTION 1
#endif
[[vk::binding(7, 0)]]
RWStructuredBuffer<float4> output_image;
[[vk::binding(8, 0)]]
RWStructuredBuffer<float4> accumulation;
[[vk::binding(9, 0)]]
RWStructuredBuffer<uint> sample_counts;
[[vk::binding(15, 0)]]
RWStructuredBuffer<float4> prefix_image;
public struct HitScene : pt::IHitScene
{
    public bool guides_enabled()
    {
        return PRIMEPT_RECONSTRUCTION != 0 && parameters[0].reconstruction.w == 2;
    }
    public uint program(HitObject object, float4 incident)
    {
        uint primitive = region_primitive(object.GetInstanceID(), object.GetPrimitiveIndex());
        return material_words.Load(material_id(primitive) * 112u + 64u) >> 2u;
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
};
[shader("closesthit")]
void shade_hit(inout pt::MaterialCall call, BuiltInTriangleIntersectionAttributes attributes)
{
    float3 a = HitTriangleVertexPosition(0), b = HitTriangleVertexPosition(1),
           c = HitTriangleVertexPosition(2);
    pt::Hit hit = {};
    hit.surface = geometry::triangle_point(a, b, c, attributes.barycentrics, ObjectToWorld3x4(),
                                           WorldToObject3x4());
    uint primitive = region_primitive(InstanceID(), PrimitiveIndex());
    float4 auxiliary = material_aux<OPENPBR_VARIANT>(primitive, attributes.barycentrics);
    uint normal = 0;
    if ((OPENPBR_VARIANT & 8u) != 0)
        normal = map_normal_xy(primitive, auxiliary.xy, b - a, c - a, (float3x3)ObjectToWorld3x4(),
                               hit.surface.normal);
    hit.identity = uint4(primitive, asuint(attributes.barycentrics), normal);
    hit.distance = RayTCurrent();
    hit.front = dot(WorldRayDirection(), hit.surface.normal) < 0;
    pt::SurfaceMaterial material = decode_surface<AtlasColor, CachedAux>(
        hit, parameters[0].block_emission_scale, { auxiliary });
    pt::shade_material<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK>(
        ModScene(), call, hit, material, WorldRayDirection(), settings(parameters[0]));
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
#if PRIMEPT_RECONSTRUCTION
    pt::DenoiseCamera current = geometry::camera_from_scaled(params.origin.xyz, params.forward.xyz,
                                                             params.right.xyz, params.up.xyz);
    pt::DenoiseCamera previous = { params.previous_position, params.previous_forward,
                                   params.previous_right, params.previous_up };
    Guides guides = { coordinate, film, current, previous };
    float distance;
    if (params.reconstruction.w == 2)
        distance = pt::trace_hit_path(HitScene(), guides, data);
    else
        distance = pt::trace_hit_path(HitScene(), pt::NoPrefixGuides(), data);
#else
    float distance = pt::trace_hit_path(HitScene(), pt::NoPrefixGuides(), data);
#endif
    prefix_image[pixel] = float4(0, 0, 0, distance);
    output_image[pixel] =
        float4(data.path.radiance, data.path.status == pt::PATH_INVALID            ? -1
                                   : data.path.status == pt::PATH_DIELECTRIC_LIMIT ? -3
                                                                                   : 0);
}
