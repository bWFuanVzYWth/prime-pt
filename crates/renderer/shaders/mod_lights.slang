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

func sample_region_quad(surface_point: geometry::SurfacePoint, uv_sample: float2, params: Params,
                        selected: light_bvh::LightWorldSample)
    ->light_sampling::LightSample
{
    var result : light_sampling::LightSample = {};
    if (selected.pmf <= 0)
        return result;
    let world = light_tree[0];
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
        distribution, emitter, vertices, surface_point, uv_sample, selected.pmf,
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
    if (world_features::configured &&
        (light_sample.emission_reference & world_features::primitive_bit) != 0)
    {
        let slot = light_sample.emission_reference & ~world_features::primitive_bit;
        let primitive = world_features::primitive_bit | (slot << 1u) | (flags & 1u);
        let bary = light_sample.emission.xy;
        if (!world_features::accepts(world_features::faces, world_features::pixels, primitive, bary,
                                     atlas))
            return 0;
        return material_emission(primitive, bary, true, block_emission_scale);
    }
    let primitive : uint = light_meta[light_sample.emission_reference].w * 2u + (flags & 1u);
    return sampled_material_emission(primitive, light_sample.emission.xy, block_emission_scale);
}
func static_quad_pdf<Area : IEmitterArea>(hit: pt::Hit, previous: float3, index: uint,
                                          selection_pmf: float)
    ->float
{
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
    return light_sampling::quad_emitter_pdf(distribution, emitter, selection_pmf, previous,
                                            hit.surface.position, hit.surface.normal,
                                            hit.identity.x & 1u, uv, emitter_flags & 3u);
}

// 叶引用为区块目录索引或当前帧的实例面。区块下的静态树独立发布、跨帧复用。
[[vk::binding(49, 0)]]
public var mixed_tree : StructuredBuffer<uint4>;
static const let selectors = 1u << 24u;
static const let leaf_bit = 0x80000000u;

func mixed_interval<let reverse : bool>(receiver: float3, selector: uint, rank: uint, header: uint4)
    ->light_bvh::SelectedInterval
{
    var remaining = selector;
    var count = selectors;
    var node = header.z;
    while ((node & leaf_bit) == 0)
    {
        let at = 4u * header.y + 3u * node;
        let a = asfloat(mixed_tree[at]);
        let b = asfloat(mixed_tree[at + 1u]);
        let children = mixed_tree[at + 2u];
        var left = count;
        if (a.w <= 0)
            left = 0u;
        else if (b.w > 0)
        {
            let da = a.xyz - receiver;
            let db = b.xyz - receiver;
            left = light_bvh::split_selectors(float2(max(dot(da, da), 1.0e-6), a.w),
                                              float2(max(dot(db, db), 1.0e-6), b.w), children.z,
                                              children.w, count);
        }
        let right = reverse ? rank >= node : remaining >= left;
        if (right)
        {
            if (!reverse)
                remaining -= left;
            count -= left;
            node = children.y;
        }
        else
        {
            count = left;
            node = children.x;
        }
        if (count == 0)
            break;
    }
    return { node & ~leaf_bit, remaining, count };
}

public func sample_quad_light(surface_point: geometry::SurfacePoint, random_sample: float3,
                              params: Params)
    ->light_sampling::LightSample
{
    let world = light_tree[0];
    let anchor = params.anchor_region.xyz;
    let receiver = surface_point.position;
    if (!world_features::configured || mixed_tree[0].x == 0)
        return sample_region_quad(
            surface_point, random_sample.xy, params,
            light_bvh::sample_light_world(world, receiver, anchor, random_sample.z));
    let header = mixed_tree[0];
    let selected =
        mixed_interval<false>(receiver, uint(random_sample.z * float(selectors)), 0u, header);
    let meta = mixed_tree[2u * header.y + selected.index];
    if ((meta.w & leaf_bit) != 0)
    {
        let index = meta.w & ~leaf_bit;
        let entry = world.regions[index];
        // 超过一个 24 位域时才另用独立维度进区块，继续给每个静态灯保留支持。
        let separate = selected.count < entry.count;
        let local = light_bvh::select_light_region(
            entry, receiver, anchor,
            separate ? uint(random_sample.x * float(selectors)) : selected.selector,
            separate ? selectors : selected.count);
        let pmf = float(local.count) / float(selectors) *
                  (separate ? float(selected.count) / float(selectors) : 1.0);
        let xy = separate
                     ? float2(float(selected.selector) / float(selected.count), random_sample.y)
                     : random_sample.xy;
        let pick : light_bvh::LightWorldSample = { index, local.index, pmf };
        return sample_region_quad(surface_point, xy, params, pick);
    }
    let slot = meta.w;
    let vertices : light_sampling::LightQuadVertices = {
        world_features::faces[slot].position[0].xyz, world_features::faces[slot].position[1].xyz,
        world_features::faces[slot].position[2].xyz, world_features::faces[slot].position[3].xyz
    };
    let emitter : light_sampling::LightSurfaceProposal = { meta, 0u };
    var area : light_sampling::QuadAreaSample;
    var result = light_sampling::sample_quad_emitter(
        light_sampling::UniformEmitterAlias(), emitter, vertices, surface_point, random_sample.xy,
        float(selected.count) / float(selectors), 3u, area);
    result.emission = float3(area.bary, asfloat(area.half));
    result.emission_reference = world_features::primitive_bit | slot;
    return result;
}

public func quad_emission_pdf<Area : IEmitterArea>(hit: pt::Hit, previous: float3, anchor: int3)
    ->float
{
    if (!world_features::active(hit.identity.x))
    {
        let index = light_index(hit.identity.x >> 1u);
        if (index == 0xffffffffu)
            return 0;
        let world = light_tree[0];
        let region = light_regions[index];
        let group = light_bvh::light_world_find_region(world, region.xyz);
        let local = asuint(region.w) >> 2u;
        var pmf : float;
        if (!world_features::configured || mixed_tree[0].x == 0)
            pmf = light_bvh::light_world_pmf(world, group, local, previous, anchor);
        else
        {
            let header = mixed_tree[0];
            let rank = mixed_tree[3u * header.y + group].x;
            let selected = mixed_interval<true>(previous, 0u, rank, header);
            let entry = world.regions[group];
            let separate = selected.count < entry.count;
            let count = light_bvh::light_region_interval_count(
                entry, local, previous, anchor, separate ? selectors : selected.count);
            pmf = float(count) / float(selectors) *
                  (separate ? float(selected.count) / float(selectors) : 1.0);
        }
        return static_quad_pdf<Area>(hit, previous, index, pmf);
    }
    let slot = world_features::slot(hit.identity.x);
    if ((world_features::faces[slot].material.y & 4u) == 0u)
        return 0;
    let id = asuint(world_features::faces[slot].previous[2].w);
    if (id == 0u)
        return 0;
    let header = mixed_tree[0];
    let rank = id - 1u;
    let meta = mixed_tree[2u * header.y + rank];
    let selected = mixed_interval<true>(previous, 0u, rank, header);
    let emitter : light_sampling::LightSurfaceProposal = { meta, 0u };
    return light_sampling::quad_emitter_pdf(
        light_sampling::UniformEmitterAlias(), emitter, float(selected.count) / float(selectors),
        previous, hit.surface.position, hit.surface.normal, hit.identity.x & 1u, float2(0), 3u);
}
