/// 在设备上从当前和上次接受的骨骼姿态生成可供全部光路共用的动态面属性。
struct Vertex
{
    float4 position_u;
    float4 v;
};
struct Pose
{
    float4 rows[3];
    float4 old_rows[3];
    uint4 mesh;
    uint4 material;
    uint4 identity;
};
struct Surface
{
    uint4 material;
    uint4 identity;
    float4 position[4];
    float4 previous[4];
    float4 uv[2];
};
[[vk::binding(0, 0)]]
var vertices : StructuredBuffer<Vertex>;
[[vk::binding(1, 0)]]
var instances : StructuredBuffer<Pose>;
[[vk::binding(2, 0)]]
var faces : RWStructuredBuffer<Surface>;
[shader("compute")]
[numthreads(32, 1, 1)]
func feature_models(group: uint3: SV_GroupID, lane: uint: SV_GroupIndex)->void
{
    let index = group.x + group.y * 65535u;
    if (index >= instances[0].mesh.x)
        return;
    let pose = instances[index + 1u];
    for (var face = lane; face < pose.mesh.y; face += 32u)
    {
        var surface : Surface;
        surface.material = pose.material;
        surface.identity = pose.identity;
        surface.identity.x += face;
        surface.identity.y += uint(surface.identity.x < pose.identity.x);
        for (var k = 0u; k < 4u; k++)
        {
            let vert = vertices[(pose.mesh.x + face) * 4u + k];
            let p = float4(vert.position_u.xyz, 1);
            surface.position[k] = float4(dot(pose.rows[0], p), dot(pose.rows[1], p),
                                         dot(pose.rows[2], p), asfloat(pose.mesh.w));
            surface.previous[k] = float4(dot(pose.old_rows[0], p), dot(pose.old_rows[1], p),
                                         dot(pose.old_rows[2], p), 0.);
            let uv = float2(vert.position_u.w, vert.v.x);
            if ((k & 1u) == 0)
                surface.uv[k >> 1u].xy = uv;
            else
                surface.uv[k >> 1u].zw = uv;
        }
        faces[pose.mesh.z + face] = surface;
    }
}
