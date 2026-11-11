/// 缓存首表面的材质专用输运；EXT不能MakeHit，因而使用callable，后续反弹仍走hit group。
module primary_material;
#include "geometry/opacity.slang"
#ifndef PRIMEPT_OMM
#define PRIMEPT_OMM 0
#endif
import primary_cache;
import known_geometry;
import mod_decode;
import pt.material_vertex;
import mod_scene;
import pt.scene;
import geometry.ray;
import params;

[shader("callable")]
void shade_primary(inout pt::MaterialCall call)
{
#if PRIMEPT_OMM
    ENGINE_OPACITY_MICROMAP(shade_primary)
#endif
    call.initializePrimary(parameters[0].initial_medium);
    uint pixel = primary_pixel();
    pt::Hit hit = {};
    hit.identity = primary_identity[pixel];
    hit.surface = geometry::surface_point(known_endpoint(hit.identity.x, asfloat(hit.identity.yz)));
    float4 ray = primary_ray[pixel];
    hit.distance = ray.w;
    hit.front = dot(ray.xyz, hit.surface.normal) < 0;
    float4 auxiliary = material_aux<OPENPBR_VARIANT>(hit.identity.x, asfloat(hit.identity.yz));
    hit.identity.w =
        known_normal_xy<OPENPBR_VARIANT>(hit.identity.x, auxiliary.xy, hit.surface.normal);
    pt::SurfaceMaterial material = decode_surface<AtlasColor, CachedAux>(
        hit, parameters[0].block_emission_scale, { auxiliary });
    pt::shade_material<OPENPBR_FLOW, ModScene, PRIMEPT_LIGHT_MASK, true>(
        ModScene(), call, hit, material, ray.xyz, settings(parameters[0]), parameters[0].image.w,
        asuint(call.origin_pixel.y));
}
