/// 缓存首表面的材质专用输运；EXT不能MakeHit，因而使用callable，后续反弹仍走hit group。
module primary_material;
#include "opacity/micromap.inc.slang"
#ifndef PRIMEPT_OMM
#define PRIMEPT_OMM 0
#endif
import primary_cache;
import known_geometry;
import mod_decode;
import nrd_path;
import pt.material_vertex;
import mod_scene;
import pt.scene;
import geometry.ray;
import params;
import transport.surface_material;

[shader("callable")]
func shade_primary(inout call: pt::MaterialCall)->void
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_primary)
#endif
    let pixel : uint = primary_pixel();
    // 虚拟终点复用输入位置携带当前世代的三角形与重心；负 PDF 仅在调度边界有效。
    let replaced = parameters[0].reconstruction.w == 3 && call.previous_pdf < 0;
    var hit : pt::Hit = {};
    hit.identity = replaced ? uint4(asuint(call.previous), 0) : primary_identity[pixel];
    let ray = replaced ? float4(call.direction, 0) : primary_ray[pixel];
    if (replaced)
        call.previous_pdf = 0;
    else
        call.initialize_primary(parameters[0].initial_medium);
    hit.surface = geometry::surface_point(known_endpoint(hit.identity.x, asfloat(hit.identity.yz)));
    hit.distance = ray.w;
    hit.front = dot(ray.xyz, hit.surface.normal) < 0;
    let auxiliary : float4 =
                        material_aux<OPENPBR_VARIANT>(hit.identity.x, asfloat(hit.identity.yz));
    hit.identity.w =
        known_normal_xy<OPENPBR_VARIANT>(hit.identity.x, auxiliary.xy, hit.surface.normal);
    let material : transport::SurfaceMaterial = decode_surface<AtlasColor, CachedAux>(
                       hit, parameters[0].block_emission_scale, { auxiliary });
    if (parameters[0].reconstruction.w == 3)
    {
        let lane = asuint(call.origin_pixel.y);
        let incident = call.medium;
        var signals : pt::PrimaryLighting;
        pt::shade_material_observed<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, true, true>(
            ModScene(), call, hit, material, ray.xyz, settings(parameters[0]), lane, signals);
        let blocker = begin_signal<OPENPBR_FLOW>(pixel * 2 + lane, signals, hit, material, incident,
                                                 -ray.xyz, call.direction, replaced);
        if (blocker == pt::FAR)
            call.radiance += signals.sun;
        return;
    }
    pt::shade_material<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, true>(
        ModScene(), call, hit, material, ray.xyz, settings(parameters[0]),
        asuint(call.origin_pixel.y));
}
