/// 发光quad的应用绑定；alias/面积/PDF和连接误差使用引擎，不拥有材质或纹理。
module mod_lights;

#ifndef PRIMEPT_EMITTER_CAPABILITIES
#define PRIMEPT_EMITTER_CAPABILITIES 7
#endif
import pt.scene;
__exported import pt.light_proposal;
import geometry.ray;
import params;
import mod_materials;

/// 首行[count,treeBase,0,0]，随后region anchor和紧凑活动槽，再存位置BVH。
/// BVH按几何发布事务更新；纹理alias仍仅在资源准备阶段构建。
[[vk::binding(30, 0)]]
StructuredBuffer<uint4> light_alias;
/// 每灯[split,rho0,rho1,globalQuadSlot]。
[[vk::binding(31, 0)]]
public StructuredBuffer<uint4> light_meta;
/// region-local四角，按light_sampling::pack_vertices编码，每灯三个float4。
[[vk::binding(32, 0)]]
public StructuredBuffer<float4> light_vertices;
/// 首行[count,mask,1,0]，后续每桶[key=globalQuadSlot+1,dataSlot+1,0,0]。
[[vk::binding(33, 0)]]
StructuredBuffer<uint4> light_inverse;
/// regionKey.xyz与(activeRank<<2)|sideMask；平移在shader按当前anchor计算。
[[vk::binding(34, 0)]]
public StructuredBuffer<int4> light_regions;
/// 每资源世代唯一的静态纹理内alias，control.w>>1给出header word偏移。
[[vk::binding(35, 0)]]
ByteAddressBuffer emission_alias_words;

/// 本帧已发布的活动灯数；策略权重不依赖单次面积proposal的接受结果。
public uint light_count()
{
    return light_alias[0].x;
}

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
        uint2 item = light_inverse[header.z + slot].xy;
        if (item.x == key)
            return item.y - 1u;
        if (item.x == 0u)
            return 0xffffffffu;
        slot = (slot + 1u) & header.y;
    }
    return 0xffffffffu;
}
public interface IEmitterArea
{
    static uint alias_offset(uint primitive);
};
public struct UniformArea : IEmitterArea
{
    public static uint alias_offset(uint primitive)
    {
        return 0;
    }
};
public struct ResourceArea : IEmitterArea
{
    public static uint alias_offset(uint primitive)
    {
        return material_words.Load(material_id(primitive) * 112u + 76u) >> 1u;
    }
};
#if PRIMEPT_EMITTER_CAPABILITIES & 1
public typedef ResourceArea PublishedArea;
#else
public typedef UniformArea PublishedArea;
#endif

// 未知命中保留已发布灯域；已绑定PDF/发光入口只组合其资源键需要的proposal。
#if defined(OPENPBR_VARIANT)
#if OPENPBR_VARIANT == 0xffffffffu || (OPENPBR_VARIANT & 1024)
public typedef PublishedArea SelectedEmitterArea;
#else
public typedef UniformArea SelectedEmitterArea;
#endif
#else
public typedef PublishedArea SelectedEmitterArea;
#endif

public pt::LightSample sample_quad_light(geometry::SurfacePoint point, float3 sample, Params params)
{
    pt::LightSample result = {};
    uint4 header = light_alias[0];
    if (header.x == 0u)
        return result;
    int3 anchor = asint(light_alias[1].xyz);
    float3 receiver = point.position + float3(params.anchor_region.xyz - anchor) * 64.0;
    pt::LightBvhProposal proposal = { light_alias, header.y, header.x };
    pt::LightBvhSample selected = proposal.sample(receiver, sample.z);
    if (selected.pmf <= 0)
        return result;
    uint index = light_alias[2u + selected.light / 4u][selected.light & 3u];
    uint4 meta = light_meta[index];
    uint primitive = meta.w * 2u;
    uint alias_offset = PublishedArea.alias_offset(primitive);
    uint start = index * 3u;
    pt::LightQuadVertices vertices = pt::light_quad_vertices(
        light_vertices[start], light_vertices[start + 1u], light_vertices[start + 2u]);
    if (meta.x == 0xffffffffu)
        meta = pt::dynamic_quad_area(vertices);
    if (asfloat(meta.y) <= 0 && asfloat(meta.z) <= 0)
        return result;
    pt::LightSurfaceProposal emitter = { meta, alias_offset };
    pt::TexturedEmitterAlias distribution = { emission_alias_words };
    pt::QuadAreaSample area = distribution.sample(emitter, sample.xy);
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
                                    selected.pmf, uint(light_regions[index].w) & 3u);
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
/// 只接受当前已发布灯提案返回且pdf>0的样本；源sideMask已完成发光侧准入。
public float3 quad_light_emission(pt::LightSample sample, float block_emission_scale)
{
    uint flags = asuint(sample.emission.z);
    uint primitive = light_meta[sample.emission_reference].w * 2u + (flags & 1u);
    return sampled_material_emission(primitive, sample.emission.xy, block_emission_scale);
}
public float quad_emission_pdf<Area : IEmitterArea>(pt::Hit hit, float3 previous,
                                                    int3 anchor_region)
{
    uint index = light_index(hit.identity.x >> 1u);
    if (index == 0xffffffffu)
        return 0;
    uint4 header = light_alias[0];
    int3 anchor = asint(light_alias[1].xyz);
    float3 receiver = previous + float3(anchor_region - anchor) * 64.0;
    pt::LightBvhProposal proposal = { light_alias, header.y, header.x };
    float pmf = proposal.pmf(receiver, uint(light_regions[index].w) >> 2u);
    uint4 meta = light_meta[index];
    uint alias_offset = Area.alias_offset(hit.identity.x);
    float2 uv = 0;
    if (alias_offset != 0u)
    {
        float4 bounds = asfloat(material_words.Load4(material_id(hit.identity.x) * 112u + 80u));
        uv = (material_uv(hit.identity.x, asfloat(hit.identity.yz)) - bounds.xy) /
             (bounds.zw - bounds.xy);
    }
    else
    {
        if (meta.x == 0xffffffffu)
        {
            uint start = index * 3u;
            meta = pt::dynamic_quad_area(pt::light_quad_vertices(
                light_vertices[start], light_vertices[start + 1u], light_vertices[start + 2u]));
        }
    }
    pt::LightSurfaceProposal emitter = { meta, alias_offset };
    pt::TexturedEmitterAlias distribution = { emission_alias_words };
    float density = distribution.density(emitter, hit.identity.x & 1u, uv);
    return pt::quad_light_pdf(previous, hit.surface.position, hit.surface.normal, density, pmf,
                              uint(light_regions[index].w) & 3u);
}
