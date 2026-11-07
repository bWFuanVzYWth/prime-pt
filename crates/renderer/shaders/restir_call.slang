/// 输入结束后复用字段保存结果；完整命中身份保持32bit。
module restir_call;
import pt.scene;
import restir_pt.scene;
__exported import restir_pt.initial_call;
import restir_pt.types;
import bsdf.openpbr.api;

public struct RestirMaterialHit
{
    public float3 normal;
    public uint4 identity;
    public uint flags;
};

public RestirMaterialHit material_hit(restir_pt::Surface surface)
{
    return { surface.point.normal, surface.identity, uint(surface.frontFacing) };
}

public pt::Hit restir_hit(restir_pt::Surface surface)
{
    pt::Hit hit = {};
    hit.surface = surface.point;
    hit.identity = surface.identity;
    hit.texel = surface.texel;
    hit.front = surface.frontFacing;
    return hit;
}

public pt::Hit to_pt_hit(RestirMaterialHit hit)
{
    pt::Hit result = {};
    result.surface.normal = hit.normal;
    result.identity = hit.identity;
    result.front = hit.flags != 0;
    return result;
}

public restir_pt::BsdfSample sampled_result(RestirMaterialHit hit, float4 medium)
{
    restir_pt::BsdfSample result = {};
    result.direction = hit.normal;
    result.weight = asfloat(hit.identity.yzw);
    result.pdf = asfloat(hit.identity.x);
    result.componentPdf = result.pdf;
    result.medium = medium;
    // point ReSTIR不消费relativeEta；不让它增加跨callable存活状态。
    result.relativeEta = 1;
    result.delta = (hit.flags & 1u) != 0;
    result.transmission = (hit.flags & 2u) != 0;
    return result;
}

public void store_sample(inout RestirMaterialHit hit, inout float4 medium, openpbr::Sample sample)
{
    hit.normal = sample.direction;
    hit.identity =
        asuint(float4(sample.pdf, sample.pdf > 0 ? sample.response / sample.pdf : float3(0)));
    hit.flags = uint((sample.events & openpbr::DISCRETE) != 0) |
                (uint((sample.events & openpbr::TRANSMISSION) != 0) << 1);
    medium = sample.medium;
}

public typealias RestirInitialCall = restir_pt::InitialSampleCall;
public typealias RestirInitialDirectCall = restir_pt::InitialKnownDirectCall;

public struct RestirReplayCall
{
    public RestirMaterialHit hit;
    public float4 incident;
    public float3 wo;
    public float3 u;
};

public struct RestirEvaluateCall
{
    public RestirMaterialHit hit;
    public float3 wo;
    public float3 direction;
    public float incident_ior;
};

/// 连接端的76B操作输入；输出复用几何法线为Beer，identity为BSDF。
public struct RestirConnectionCall
{
    public RestirMaterialHit hit;
    public float3 wo;
    public float3 direction;
    public float4 incident;
    public float distance;
};

/// 元数据输出共48B；输入复用first存原始身份，second.x存面朝向。
public struct RestirMetadataCall
{
    public float4 first;
    public float4 second;
    public float4 third;

    public static RestirMetadataCall input(uint4 identity, bool front)
    {
        RestirMetadataCall result = {};
        result.first = asfloat(identity);
        result.second.x = asfloat(uint(front));
        return result;
    }

    [mutating]
    public void store(pt::SurfaceMaterial material, float4 incident, bool continuous)
    {
        first = float4(material.pbr.base_roughness.w,
                       asfloat(uint3(material.pbr.control.x, asuint(material.pbr.surface.y),
                                     uint(continuous))));
        second = material.transmitted;
        third = incident;
    }

    public void apply(inout restir_pt::Surface surface)
    {
        surface.emission = 0;
        surface.roughness = first.x;
        surface.transmitted = second;
        surface.incident = third;
        surface.capability = asuint(first.y);
        surface.material = asuint(first.z);
        surface.hasNonDeltaLobes = asuint(first.w) != 0;
    }
};

/// 28B逆向提案输入；返回时identity.x复用为单个PDF，不跨call传几何。
public struct RestirPdfCall
{
    public uint4 identity;
    public float3 previous;
};

/// 40B发光端结果；输入前28B复用为稳定HitInfo和前驱位置，不传已解码几何/材料。
public struct RestirEmitterCall
{
    public float3 emission;
    public float pdf;
    public float3 position;
    public float3 normal;

    public static RestirEmitterCall input(restir_pt::HitInfo hit, float3 from, float3 wo)
    {
        return { asfloat(hit.data.xyz), asfloat(hit.data.w), from, wo };
    }

    public restir_pt::HitInfo identity()
    {
        return { asuint(float4(emission, pdf)) };
    }
};
