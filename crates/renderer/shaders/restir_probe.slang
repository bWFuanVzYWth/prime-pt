/// 固定32×32测试输入的重投影读回，调用者提供同尺寸输出；不维护另一份地址公式。
module restir_probe;
import restir;
import restir_scene;
import restir_pt.types;
import restir_pt.scene;
import mod_scene;
import params;

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
    RestirScene scene;
    donor_output[1] =
        uint4(restir_valid_hit(hit), scene.loadSurface(hit, float3(0, 0, -1), true).hit.isValid(),
              scene.loadSurfaceFromOrigin(hit, float3(0), true).hit.isValid(),
              scene.loadSurface(hit, float3(0, 0, -1), true).identity.w);
}

/// 只导出生产paired地址，CPU以独立f64公式核对颜色，不复制邻居调度。
[shader("compute")]
[numthreads(16, 16, 1)]
void restir_cv_neighbors(uint3 id: SV_DispatchThreadID)
{
    donor_output[id.y * 32u + id.x] = cv_spatial_indices(id.xy);
}

/// 实际场景的两族条件密度、逆向密度与方向；固定32x32输入。
[shader("compute")]
[numthreads(16, 16, 1)]
void restir_light_family_probe(uint3 id: SV_DispatchThreadID)
{
    RestirScene scene;
    restir_pt::Surface surface =
        scene.loadSurfaceFromOrigin(primary(id.xy), parameters[0].origin.xyz, false);
    float3 u = (float3(id.xy, id.x ^ id.y) + 0.5) / 32.0;
    restir_pt::LightSample local = scene.sampleLight(surface, float3(u.xy, u.z * 0.5));
    restir_pt::LightSample sun = scene.sampleLight(surface, float3(u.xy, 0.5 + u.z * 0.5));
    uint offset = 2u * (id.y * 32u + id.x);
    donor_output[offset] = asuint(float4(
        local.pdf, scene.lightPdf(surface, local.hit, local.direction, local.lightType, false),
        sun.pdf, scene.lightPdf(surface, sun.hit, sun.direction, sun.lightType, false)));
    donor_output[offset + 1u] =
        uint4(local.lightType, sun.lightType,
              asuint(dot(sun.direction, parameters[0].lighting.xyz)), primary(id.xy).data.x);
}
