/// 仅初采拥有实际HitObject；历史表面继续使用restir_scene的callable入口。
module initial_hit;
import restir_scene;
import restir_call;
import restir_pt.initial;
import restir_pt.scene;
import restir_pt.types;
import mod_scene;
import params;
import pt.scene;
import geometry.ray;

public struct InitialHitTrace : restir_pt::IInitialTrace<RestirScene, HitObject>
{

    [mutating]
    public bool trace(inout RestirScene provider, inout HitObject found, float3 origin,
                      float3 direction, out restir_pt::HitInfo hit, out float distance)
    {
        RayDesc ray = { origin, 0, direction, pt::FAR };
        uint unused = 0;
        found = HitObject::TraceRay(scene, RAY_FLAG_NONE, 0xff, 0, 1, 0, ray, unused);
        hit = {};
        distance = pt::FAR;
        if (!found.IsHit())
            return false;
        BuiltInTriangleIntersectionAttributes attributes =
            found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        hit = restir_stable_hit(region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex()),
                                attributes.barycentrics);
        distance = found.GetRayDesc().TMax;
        return true;
    }
    public restir_pt::Surface load(inout RestirScene provider, inout HitObject found,
                                   restir_pt::HitInfo hit, float3 view, bool fromOrigin)
    {
#if defined(PRIMEPT_HIT_GEOMETRY) || defined(PRIMEPT_HIT_GEOMETRY_LOAD)
        BuiltInTriangleIntersectionAttributes attributes =
            found.GetAttributes<BuiltInTriangleIntersectionAttributes>();
        float3 v[3] = found.GetTriangleVertexPositions();
        pt::Hit source = {};
        source.surface = geometry::triangle_point(v[0], v[1], v[2], attributes.barycentrics,
                                                  transpose(found.GetObjectToWorld()),
                                                  transpose(found.GetWorldToObject()));
        source.identity = uint4(region_primitive(found.GetInstanceID(), found.GetPrimitiveIndex()),
                                asuint(attributes.barycentrics), 0);
        float3 delta = fromOrigin ? view - source.surface.position : view;
        restir_pt::Surface result =
            provider.surface_from_hit(hit, source, fromOrigin ? normalize(delta) : view);
        if (fromOrigin)
            result.distance = length(delta);
        return result;
#else
        return fromOrigin ? provider.loadSurfaceFromOrigin(hit, view, false)
                          : provider.loadSurface(hit, view, false);
#endif
    }

    [mutating]
    public restir_pt::InitialShading shade(inout RestirScene provider, inout HitObject found,
                                           restir_pt::Surface surface, float3 u, float3 direction,
                                           float3 secondDirection, bool continuation)
    {
        uint program = (materials[material_id(surface.identity.x)].control.x >> 2u) +
                       (continuation ? 0u : asuint(parameters[0].restir.z));
        found.SetShaderTableIndex(program + 1u);
        if (continuation)
        {
            RestirInitialCall call = {};
#if !defined(PRIMEPT_HIT_GEOMETRY) && !defined(PRIMEPT_HIT_GEOMETRY_LEAF)
            call.hit = material_hit(surface);
            call.wo = surface.wo;
#endif
            call.incident = surface.incident;
            call.u = u;
            call.direction = direction;
            call.second_direction = secondDirection;
#ifdef PRIMEPT_SER
#ifdef PRIMEPT_HIT_HINT
            ReorderThread(program, PRIMEPT_SER_BITS);
#else
            ReorderThread(found, 0u, 0u);
#endif
#endif
            HitObject::Invoke(found, call);
            return call.result();
        }
        RestirDirectCall call = {};
#if !defined(PRIMEPT_HIT_GEOMETRY) && !defined(PRIMEPT_HIT_GEOMETRY_LEAF)
        call.hit = material_hit(surface);
        call.wo = surface.wo;
#endif
        call.direction = direction;
        call.incident_ior = surface.incident.x;
        call.second_direction = secondDirection;
#ifdef PRIMEPT_SER
#ifdef PRIMEPT_HIT_HINT
        ReorderThread(program, PRIMEPT_SER_BITS);
#else
        ReorderThread(found, 0u, 0u);
#endif
#endif
        HitObject::Invoke(found, call);
        return { {},
                 { asfloat(call.hit.identity.yzw), asfloat(call.hit.identity.x) },
                 call.hit.normal,
                 { call.wo, call.direction.x } };
    }
};
