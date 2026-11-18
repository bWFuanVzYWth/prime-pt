/// 发光quad的应用绑定；alias/面积/PDF和连接误差使用引擎，不拥有材质或纹理。
module mod_lights;

#ifndef PRIMEPT_EMITTER_CAPABILITIES
#define PRIMEPT_EMITTER_CAPABILITIES 7
#endif
import pt.scene;
import geometry.ray;
import params;
import mod_materials;
import light_bvh.sample;
import light_bvh.world;
import light_sampling.area;
import light_sampling.quad;
import light_sampling.sample;

/// 世界树引用可独立复用的区块子树。区块叶使用稳定灯槽；纹理 alias 属于资源世代。
[[vk::binding(38, 0)]]
var light_tree : StructuredBuffer<light_bvh::LightWorld>;
/// 每灯[split,rho0,rho1,globalQuadSlot]。
[[vk::binding(31, 0)]]
public var light_meta : StructuredBuffer<uint4>;
/// 与 BLAS 共读的四顶点列，发光目录只保存其 quad 槽。
[[vk::binding(32, 0)]]
public var light_vertices : StructuredBuffer<float4>;
/// 首行[count,mask,1,0]，后续每桶[key=globalQuadSlot+1,dataSlot+1,0,0]。
[[vk::binding(33, 0)]]
var light_inverse : StructuredBuffer<uint4>;
/// regionKey.xyz 与 (localIndex<<2)|sideMask；局部编号不随其他区块变化。
[[vk::binding(34, 0)]]
public var light_regions : StructuredBuffer<int4>;
/// 每资源世代唯一的静态纹理内alias，control.w>>1给出header word偏移。
[[vk::binding(35, 0)]]
var emission_alias_words : ByteAddressBuffer;

public func emitter_vertices(index: uint)->light_sampling::LightQuadVertices
{
    let at : uint = light_meta[index].w * 4u;
    return { light_vertices[at].xyz, light_vertices[at + 1u].xyz, light_vertices[at + 2u].xyz,
             light_vertices[at + 3u].xyz };
}
/// 本帧已发布的活动灯数；策略权重不依赖单次面积proposal的接受结果。
public func light_count()->uint
{
    return light_tree[0].counts.x;
}

func light_hash(input_key: uint)->uint
{
    var key = input_key;
    key ^= key >> 16u;
    key *= 0x7feb352du;
    key ^= key >> 15u;
    key *= 0x846ca68bu;
    return key ^ (key >> 16u);
}
func light_index(quad: uint)->uint
{
    let header : uint4 = light_inverse[0];
    if (header.x == 0)
        return 0xffffffffu;
    let key : uint = quad + 1u;
    var slot : uint = light_hash(key) & header.y;
    for (var probe: uint = 0; probe <= header.y; ++probe)
    {
        let item : uint2 = light_inverse[header.z + slot].xy;
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
    static func alias_offset(primitive: uint)->uint;
};
public struct UniformArea : IEmitterArea
{
    public static func alias_offset(primitive: uint)->uint
    {
        return 0;
    }
};
public struct ResourceArea : IEmitterArea
{
    public static func alias_offset(primitive: uint)->uint
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

public func sample_quad_light(surface_point: geometry::SurfacePoint, random_sample: float3,
                              params: Params)
    ->light_sampling::LightSample
{
    var result : light_sampling::LightSample = {};
    let world = light_tree[0];
    let selected = light_bvh::sample_light_world(world, surface_point.position,
                                                 params.anchor_region.xyz, random_sample.z);
    if (selected.pmf <= 0)
        return result;
    let region = world.regions[selected.region];
    let slots = (uint *)(region.words + light_bvh::region_payload(region.count));
    let index = slots[selected.local];
    var meta : uint4 = light_meta[index];
    let primitive : uint = meta.w * 2u;
    let alias_offset : uint = PublishedArea.alias_offset(primitive);
    let vertices : light_sampling::LightQuadVertices = emitter_vertices(index);
    if (meta.x == 0xffffffffu)
        meta = light_sampling::dynamic_quad_area(vertices);
    if (asfloat(meta.y) <= 0 && asfloat(meta.z) <= 0)
        return result;
    let emitter : light_sampling::LightSurfaceProposal = { meta, alias_offset };
    let distribution : light_sampling::TexturedEmitterAlias = { emission_alias_words };
    var area : light_sampling::QuadAreaSample;
    let translation = float3(light_regions[index].xyz - params.anchor_region.xyz) * 64.0;
    var transform =
        float3x4(1, 0, 0, translation.x, 0, 1, 0, translation.y, 0, 0, 1, translation.z);
    let inverse =
        float3x4(1, 0, 0, -translation.x, 0, 1, 0, -translation.y, 0, 0, 1, -translation.z);
    result = light_sampling::sample_quad_emitter(
        distribution, emitter, vertices, surface_point, random_sample.xy, selected.pmf,
        uint(light_regions[index].w) & 3u, transform, inverse, area);
    // 遮挡确认后才按实际 bary 读取发光纹理。
    result.emission = float3(area.bary, asfloat(area.half));
    result.emission_reference = index;
    return result;
}
/// 只接受当前已发布灯提案返回且pdf>0的样本；源sideMask已完成发光侧准入。
public func quad_light_emission(light_sample: light_sampling::LightSample,
                                block_emission_scale: float)
    ->float3
{
    let flags : uint = asuint(light_sample.emission.z);
    let primitive : uint = light_meta[light_sample.emission_reference].w * 2u + (flags & 1u);
    return sampled_material_emission(primitive, light_sample.emission.xy, block_emission_scale);
}
public func quad_emission_pdf<Area : IEmitterArea>(hit: pt::Hit, previous: float3,
                                                   anchor_region: int3)
    ->float
{
    let index : uint = light_index(hit.identity.x >> 1u);
    if (index == 0xffffffffu)
        return 0;
    let world = light_tree[0];
    var meta : uint4 = light_meta[index];
    let alias_offset : uint = Area.alias_offset(hit.identity.x);
    var uv : float2 = 0;
    if (alias_offset != 0u)
    {
        let bounds : float4 =
                         asfloat(material_words.Load4(material_id(hit.identity.x) * 112u + 80u));
        uv = (material_uv(hit.identity.x, asfloat(hit.identity.yz)) - bounds.xy) /
             (bounds.zw - bounds.xy);
    }
    else
    {
        if (meta.x == 0xffffffffu)
        {
            meta = light_sampling::dynamic_quad_area(emitter_vertices(index));
        }
    }
    let emitter : light_sampling::LightSurfaceProposal = { meta, alias_offset };
    let distribution : light_sampling::TexturedEmitterAlias = { emission_alias_words };
    let region = light_regions[index];
    let emitter_flags = asuint(region.w);
    let group = light_bvh::light_world_find_region(world, region.xyz);
    let selection_pmf =
        light_bvh::light_world_pmf(world, group, emitter_flags >> 2u, previous, anchor_region);
    return light_sampling::quad_emitter_pdf(distribution, emitter, selection_pmf, previous,
                                            hit.surface.position, hit.surface.normal,
                                            hit.identity.x & 1u, uv, emitter_flags & 3u);
}
