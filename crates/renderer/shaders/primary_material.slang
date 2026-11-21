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
    call.initialize_primary(parameters[0].initial_medium);
    let pixel : uint = primary_pixel();
    var hit : pt::Hit = {};
    let cached = primary_hit[pixel];
    hit.identity = uint4(cached.xyz, 0);
    hit.surface = geometry::surface_point(known_endpoint(hit.identity.x, asfloat(hit.identity.yz)));
    let direction = primary_direction(parameters[0], DispatchRaysIndex().xy);
    hit.distance = asfloat(cached.w);
    hit.front = dot(direction, hit.surface.normal) < 0;
    let auxiliary = material_aux<OPENPBR_VARIANT>(hit.identity.x, asfloat(hit.identity.yz));
    hit.identity.w = known_normal_xy<OPENPBR_VARIANT>(hit.identity.x, auxiliary.normal_ndf.xy,
                                                      hit.surface.normal);
    let material : transport::SurfaceMaterial = decode_surface<AtlasColor, CachedAux>(
                       hit, parameters[0].block_emission_scale, { auxiliary });
    if (parameters[0].reconstruction.w == 3)
    {
        let lane = asuint(call.origin_pixel.y);
        let incident = call.medium;
        var signals : pt::PrimaryLighting;
        pt::shade_material_observed<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, true, true>(
            ModScene(), call, hit, material, direction, settings(parameters[0]), lane, signals);
        let blocker = begin_signal<OPENPBR_FLOW>(pixel * 2 + lane, signals, hit, material, incident,
                                                 -direction, call.previous_pdf);
        if (blocker == pt::FAR)
            call.radiance += signals.sun;
        return;
    }
    pt::shade_material<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, true>(
        ModScene(), call, hit, material, direction, settings(parameters[0]),
        asuint(call.origin_pixel.y));
}
