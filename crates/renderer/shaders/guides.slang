/// PT与ReSTIR共用的独立导引调度；SBT只含几何占位和精确Guide材质命中组。
module guides;
import mod_guides;
import mod_scene;
import pt.key_guides;
import pt.denoise;
import geometry.camera;
import params;
import primary_cache;

[shader("raygeneration")]
void guide_main()
{
    Params p = parameters[0];
    uint2 pixel = DispatchRaysIndex().xy;
    float2 film = (float2(pixel) + 0.5 + p.reconstruction.xy) / float2(p.image.xy);
    float3 direction =
        geometry::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
    GuideTrace trace = { false };
    pt::KeyGuideVertex first;
    bool found;
    if (realtime_pt())
    {
        uint index = primary_pixel();
        uint primitive = primary_identity[index].x;
        direction = primary_ray[index].xyz;
        found = primitive != 0xffffffffu;
        first = {};
        if (found)
        {
            uint program = guide_slot(primitive);
            pt::KeyGuideCall call = {};
            call.value.medium = p.initial_medium;
#ifdef PRIMEPT_SER
            ReorderThread(program, 32u);
#endif
            CallShader(program, call);
            first = call.value;
        }
    }
    else
        found = trace.intersect_guide(p.origin.xyz, direction, p.initial_medium, first);
    pt::DenoiseCamera current =
        geometry::camera_from_scaled(p.origin.xyz, p.forward.xyz, p.right.xyz, p.up.xyz);
    pt::DenoiseCamera previous = { p.previous_position, p.previous_forward, p.previous_right,
                                   p.previous_up };
    Guides guides = { pixel, film, current, previous };
    if (found)
        guides.write_key(trace, first, p.origin.xyz, direction, p.initial_medium, realtime_pt());
    else
        guides.sky(direction);
}
