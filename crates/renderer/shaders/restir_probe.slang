/// 固定32×32测试输入的重投影读回，调用者提供同尺寸输出；不维护另一份地址公式。
module restir_probe;
import restir;
import restir_scene;
import restir_pt.types;
import restir_pt.scene;
import mod_scene;
import params;
import pt.scene;
import geometry.ray;

[[vk::binding(47, 0)]]
RWStructuredBuffer<uint4> donor_output;

[shader("compute")]
[numthreads(16, 16, 1)]
void restir_donor_probe(uint3 pixel: SV_DispatchThreadID)
{
    uint2 donor;
    bool valid = temporal_donor(pixel.xy, donor);
    donor_output[pixel.y * 32u + pixel.x] =
        uint4(valid ? donor : uint2(0xffffffffu), parameters[0].temporal_reserved.y,
              primary(pixel.xy).data.x);
}

[shader("compute")]
[numthreads(1, 1, 1)]
void restir_identity_probe()
{
    restir_pt::HitInfo hit = { donor_output[0] };
    RestirScene scene = {};
    restir_pt::Surface source = scene.loadGeometrySurface(hit, float3(0, 0, -1));
    uint normal = 0;
    if (source.hit.isValid() && (material_at(material_id(source.identity.x)).control.w & 1u) != 0)
        normal =
            restir_normal<8u>(source.identity.x, asfloat(source.identity.yz), source.point.normal);
    donor_output[1] =
        uint4(restir_valid_hit(hit), source.hit.isValid(),
              scene.loadGeometrySurfaceFromOrigin(hit, float3(0)).hit.isValid(), normal);
}

/// 只导出生产paired地址，CPU以独立f64公式核对颜色，不复制邻居调度。
[shader("compute")]
[numthreads(16, 16, 1)]
void restir_cv_neighbors(uint3 id: SV_DispatchThreadID)
{
    donor_output[id.y * 32u + id.x] = cv_spatial_indices(id.xy);
}

// compute参考直接消费同源逆向提案数学；生产RT入口按pdf角色分派。
float probe_light_pdf(RestirScene scene, restir_pt::Surface source, restir_pt::LightSample light)
{
    if (light.lightType != restir_pt::LIGHT_EMISSIVE)
        return scene.environmentPdf(light.direction);
    uint primitive = restir_primitive(light.hit);
    if (primitive == 0xffffffffu)
        return 0;
    pt::Hit hit = {};
    hit.identity = uint4(primitive, light.hit.data.zw, 0);
    hit.surface = geometry::surface_point(restir_endpoint(primitive, asfloat(light.hit.data.zw)));
    return ModScene().emission_pdf(hit, source.point.position);
}

/// 实际场景的两族条件密度、逆向密度与方向；固定32x32输入。
[shader("compute")]
[numthreads(16, 16, 1)]
void restir_light_family_probe(uint3 id: SV_DispatchThreadID)
{
    RestirScene scene = {};
    restir_pt::Surface surface =
        scene.loadGeometrySurfaceFromOrigin(primary(id.xy), parameters[0].origin.xyz);
    float3 u = (float3(id.xy, id.x ^ id.y) + 0.5) / 32.0;
    restir_pt::LightSample local = scene.sampleLight(surface, float3(u.xy, u.z * 0.5));
    restir_pt::LightSample sun = scene.sampleLight(surface, float3(u.xy, 0.5 + u.z * 0.5));
    uint offset = 2u * (id.y * 32u + id.x);
    donor_output[offset] = asuint(float4(local.pdf, probe_light_pdf(scene, surface, local), sun.pdf,
                                         probe_light_pdf(scene, surface, sun)));
    donor_output[offset + 1u] =
        uint4(local.lightType, sun.lightType,
              asuint(dot(sun.direction, parameters[0].lighting.xyz)), primary(id.xy).data.x);
}
