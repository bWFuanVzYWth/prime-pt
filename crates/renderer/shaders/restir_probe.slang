/// 固定32×32测试输入的重投影读回，调用者提供同尺寸输出；不维护另一份地址公式。
module restir_probe;
import restir;
import restir_scene;
import restir_pt.types;
import mod_scene;
import params;

[[vk::binding(45, 0)]]
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
