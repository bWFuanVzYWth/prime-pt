/// 输入结束后复用字段保存结果；完整命中身份保持32bit。
module restir_call;
import pt.scene;
import restir_pt.scene;
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

/// 96B双向求值与延续；返回时复用wo/u.xy存PDF，两个direction存第二响应/发光。
public struct RestirInitialCall
{
    public RestirMaterialHit hit;
    public float4 incident;
    public float3 wo;
    public float3 u;
    public float3 direction;
    public float3 second_direction;

    public restir_pt::InitialShading result()
    {
        return { sampled_result(hit, incident), { wo, u.x }, second_direction, { direction, u.y } };
    }
};

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

/// 不采样延续时没有随机向量、介质RGB或sample输出。
public struct RestirDirectCall
{
    public RestirMaterialHit hit;
    public float3 wo;
    public float3 direction;
    public float incident_ior;
    public float3 second_direction;
};
