/// 初采只加载几何，完整材质由一次shade产生；可选HitObject只属于当前新命中。
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

public struct ModInitialTrace : restir_pt::IInitialTrace<RestirScene, uint>
{
    [mutating]
    public bool trace(inout RestirScene provider, inout uint state, float3 origin, float3 direction,
                      out restir_pt::HitInfo hit, out float distance)
    {
        return provider.trace(origin, direction, false, hit, distance);
    }
    public restir_pt::Surface load(inout RestirScene provider, inout uint state,
                                   restir_pt::HitInfo hit, float3 view, bool fromOrigin)
    {
        return fromOrigin ? provider.loadGeometrySurfaceFromOrigin(hit, view)
                          : provider.loadGeometrySurface(hit, view);
    }
    [mutating]
    public restir_pt::InitialVertex shade(inout RestirScene provider, inout uint state,
                                          restir_pt::Surface surface,
                                          restir_pt::InitialRequest request)
    {
        return provider.shadeInitialVertex(surface, request);
    }
};

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
            provider.geometry_surface_from_hit(hit, source, fromOrigin ? normalize(delta) : view);
        if (fromOrigin)
            result.distance = length(delta);
        return result;
#else
        return fromOrigin ? provider.loadGeometrySurfaceFromOrigin(hit, view)
                          : provider.loadGeometrySurface(hit, view);
#endif
    }

    [mutating]
    public restir_pt::InitialVertex shade(inout RestirScene provider, inout HitObject found,
                                          restir_pt::Surface surface,
                                          restir_pt::InitialRequest request)
    {
        bool continuation = (request.flags & restir_pt::INITIAL_SAMPLE) != 0;
        uint program = material_program(surface.identity.x,
                                        continuation ? MATERIAL_INITIAL : MATERIAL_INITIAL_DIRECT);
        found.SetShaderTableIndex(program + 1u);
#ifdef PRIMEPT_SER
#ifdef PRIMEPT_HIT_HINT
        ReorderThread(program, PRIMEPT_SER_BITS);
#else
        ReorderThread(found, 0u, 0u);
#endif
#endif
        if (continuation)
        {
#ifdef PRIMEPT_HIT_GEOMETRY_LEAF
            RestirInitialCall call = {};
            call.vertex = restir_pt::InitialPacket::inputHit(surface.incident, request);
#else
            RestirInitialCall call = RestirInitialCall::input(surface, request, surface.identity);
#endif
            HitObject::Invoke(found, call);
            return call.result();
        }
#ifdef PRIMEPT_HIT_GEOMETRY_LEAF
        restir_pt::InitialHitDirectCall direct = { restir_pt::InitialPacket::inputHit(
            surface.incident, request) };
#else
        RestirInitialDirectCall direct =
            RestirInitialDirectCall::input(surface, request, surface.identity);
#endif
        HitObject::Invoke(found, direct);
        return direct.result();
    }
};
