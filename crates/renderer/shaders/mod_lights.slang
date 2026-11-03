/// 发光quad的应用绑定；alias/面积/PDF和连接误差使用引擎，不拥有材质或纹理。
module mod_lights;
import pt.scene;
import pt.light_sampling;
import pt.emission_alias;
import geometry.ray;
import params;
import mod_materials;

/// 活动registry首行[count,0,0,0]；运行期无全局alias构建。
[[vk::binding(30, 0)]]
StructuredBuffer<uint4> light_alias;
/// 每灯[split,rho0,rho1,globalQuadSlot]。
[[vk::binding(31, 0)]]
public StructuredBuffer<uint4> light_meta;
/// region-local四角，按light_sampling::pack_vertices编码，每灯三个float4。
[[vk::binding(32, 0)]]
public StructuredBuffer<float4> light_vertices;
/// scene_cache::directory稀疏目录，identity=globalQuadSlot+1，entry.y=lightIndex。
[[vk::binding(33, 0)]]
StructuredBuffer<uint4> light_inverse;
/// regionKey.xyz与发射sideMask；平移在shader按当前anchor计算。
[[vk::binding(34, 0)]]
public StructuredBuffer<int4> light_regions;
/// 每资源世代唯一的静态纹理内alias，control.w>>1给出header word偏移。
[[vk::binding(35, 0)]]
StructuredBuffer<uint> emission_alias_words;

uint light_hash(uint key)
{
    key ^= key >> 16u;
    key *= 0x7feb352du;
    key ^= key >> 15u;
    key *= 0x846ca68bu;
    return key ^ (key >> 16u);
}
uint light_index(uint quad)
{
    uint4 header = light_inverse[0];
    if (header.x == 0)
        return 0xffffffffu;
    uint key = quad + 1u;
    uint slot = light_hash(key) & header.y;
    for (uint probe = 0; probe <= header.y; ++probe)
    {
        uint4 packed = light_inverse[header.z + slot / 2u];
        uint2 item = (slot & 1u) == 0u ? packed.xy : packed.zw;
        if (item.x == key)
            return light_inverse[item.y].y;
        if (item.x == 0u)
            return 0xffffffffu;
        slot = (slot + 1u) & header.y;
    }
    return 0xffffffffu;
}
public pt::LightSample sample_quad_light(geometry::SurfacePoint point, float3 sample, Params params)
{
    pt::LightSample result = {};
    uint4 header = light_alias[0];
    pt::LightAliasSample selected = pt::uniform_light_sample(header.x, sample.z);
    if (selected.pmf <= 0)
        return result;
    uint index = selected.index;
    uint4 meta = light_meta[index];
    uint primitive = meta.w * 2u;
    uint alias_offset = materials[material_id(primitive)].control.w >> 1u;
    uint start = index * 3u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(
        light_vertices[start], light_vertices[start + 1u], light_vertices[start + 2u]);
    if (meta.x == 0xffffffffu)
        meta = pt::dynamic_quad_area(vertices);
    if (asfloat(meta.y) <= 0 && asfloat(meta.z) <= 0)
        return result;
    pt::QuadAreaSample area =
        alias_offset != 0u ? pt::sample_emitter_alias(emission_alias_words, alias_offset, sample.xy)
                           : pt::sample_quad_area(meta, sample.xy);
    float3 translation = float3(light_regions[index].xyz - params.anchor_region.xyz) * 64.0;
    float3x4 transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    float3x4 inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    geometry::TriangleEndpoint target = pt::quad_endpoint(vertices, area, transform, inverse);
    float3 delta = target.position - point.position;
    float distance_squared = dot(delta, delta);
    if (!(distance_squared > 0) || !isfinite(distance_squared))
        return result;
    result.direction = delta * rsqrt(distance_squared);
    result.pdf = pt::quad_light_pdf(point.position, target.position, target.normal, area.density,
                                    selected.pmf, uint(light_regions[index].w));
    if (result.pdf <= 0 || !geometry::connection_ray(point, target, result.visibility))
    {
        result.pdf = 0;
        return result;
    }
    result.distance = sqrt(distance_squared);
    bool front = dot(target.normal, -result.direction) >= 0;
    // 此私有延迟参数仅由quad_light_emission解释，避免在遮挡之前读取发光贴图。
    result.emission = float3(area.bary, asfloat(area.half | (uint(front) << 1u)));
    result.emission_reference = index;
    return result;
}
public float3 quad_light_emission(pt::LightSample sample)
{
    uint flags = asuint(sample.emission.z);
    uint primitive = light_meta[sample.emission_reference].w * 2u + (flags & 1u);
    return material_emission(primitive, sample.emission.xy, (flags & 2u) != 0);
}
public float quad_emission_pdf(pt::Hit hit, float3 previous)
{
    uint index = light_index(hit.identity.x >> 1u);
    if (index == 0xffffffffu)
        return 0;
    uint4 header = light_alias[0];
    float pmf = pt::uniform_light_pmf(header.x, index);
    uint4 meta = light_meta[index];
    ModMaterial material = materials[material_id(hit.identity.x)];
    uint alias_offset = material.control.w >> 1u;
    float density;
    if (alias_offset != 0u)
    {
        float2 uv =
            (material_uv(hit.identity.x, asfloat(hit.identity.yz)) - material.uv_bounds.xy) /
            (material.uv_bounds.zw - material.uv_bounds.xy);
        density = pt::emitter_alias_density(emission_alias_words, alias_offset, uv);
    }
    else
    {
        if (meta.x == 0xffffffffu)
        {
            uint start = index * 3u;
            meta = pt::dynamic_quad_area(pt::light_quad_vertices(
                light_vertices[start], light_vertices[start + 1u], light_vertices[start + 2u]));
        }
        density = pt::quad_area_density(meta, hit.identity.x & 1u);
    }
    return pt::quad_light_pdf(previous, hit.surface.position, hit.surface.normal, density, pmf,
                              uint(light_regions[index].w));
}
