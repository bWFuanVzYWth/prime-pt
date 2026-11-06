/// 输入结束后复用字段保存结果；完整命中身份保持32bit。
module restir_call;
import pt.scene;
import restir_pt.scene;
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

/// 224B复用输入与数值结果；跨call不保存材料闭包、灯采样器或RIS状态。
public struct RestirInitialCall
{
    public RestirMaterialHit hit;
    public float4 incident;
    public float3 emission;
    public restir_pt::InitialDirect first;
    public restir_pt::InitialDirect second;
    public float emissionPdf;

    public static RestirInitialCall input(restir_pt::Surface surface,
                                          restir_pt::InitialRequest request)
    {
        RestirInitialCall result = {};
        result.hit = material_hit(surface);
        result.incident = surface.incident;
        result.emission = surface.wo;
        result.first.hit.data.xy = uint2(request.bounceSeed, request.flags);
        result.first.direction = surface.point.position;
        result.first.radiance.x = surface.point.offset;
        result.first.reflected = request.previousPosition;
        return result;
    }

    public restir_pt::Surface surface()
    {
        restir_pt::Surface result = {};
        result.point = { first.direction, hit.normal, first.radiance.x };
        result.identity = hit.identity;
        result.frontFacing = hit.flags != 0;
        result.incident = incident;
        result.wo = emission;
        return result;
    }

    public restir_pt::InitialRequest request()
    {
        return { first.hit.data.x, first.hit.data.y, first.reflected };
    }

    [mutating]
    public void store(restir_pt::InitialVertex value)
    {
        hit.normal = value.sample.direction;
        hit.identity = asuint(float4(value.sample.pdf, value.sample.weight));
        hit.flags = uint(value.sample.delta) | (uint(value.sample.transmission) << 1u);
        incident = value.sample.medium;
        emission = value.emission;
        first = value.first;
        second = value.second;
        emissionPdf = value.emissionPdf;
    }

    public restir_pt::InitialVertex result()
    {
        return { sampled_result(hit, incident), emission, emissionPdf, first, second };
    }
};

public struct RestirReplayCall
{
    public RestirMaterialHit hit;
    public float4 incident;
    public float3 wo;
    public float3 u;
};

/// 176B初采无延续入口；不跨call存放必然不会消费的48B散射结果。
public struct RestirInitialDirectCall
{
    public float3 emission;
    public float emissionPdf;
    public restir_pt::InitialDirect first;
    public restir_pt::InitialDirect second;

    public static RestirInitialDirectCall input(restir_pt::Surface surface,
                                                restir_pt::InitialRequest request)
    {
        RestirInitialDirectCall result = {};
        result.emission = surface.wo;
        result.emissionPdf = asfloat(uint(surface.frontFacing));
        result.first.hit.data = surface.identity;
        result.first.direction = surface.point.position;
        result.first.radiance = surface.point.normal;
        result.first.reflected = surface.incident.xyz;
        result.first.attenuatedRadiance =
            float3(surface.incident.w, surface.point.offset, asfloat(request.bounceSeed));
        result.first.pdf = asfloat(request.flags & ~restir_pt::INITIAL_SAMPLE);
        result.second.direction = request.previousPosition;
        return result;
    }

    public restir_pt::Surface surface()
    {
        restir_pt::Surface result = {};
        result.point = { first.direction, first.radiance, first.attenuatedRadiance.y };
        result.identity = first.hit.data;
        result.frontFacing = asuint(emissionPdf) != 0;
        result.incident = float4(first.reflected, first.attenuatedRadiance.x);
        result.wo = emission;
        return result;
    }

    public restir_pt::InitialRequest request()
    {
        return { asuint(first.attenuatedRadiance.z), asuint(first.pdf) & ~restir_pt::INITIAL_SAMPLE,
                 second.direction };
    }

    [mutating]
    public void store(restir_pt::InitialVertex value)
    {
        emission = value.emission;
        emissionPdf = value.emissionPdf;
        first = value.first;
        second = value.second;
    }

    /// 调用者已明确不延续，sample域无消费者并规范为零。
    public restir_pt::InitialVertex result()
    {
        restir_pt::InitialVertex value = {};
        value.emission = emission;
        value.emissionPdf = emissionPdf;
        value.first = first;
        value.second = second;
        return value;
    }
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
