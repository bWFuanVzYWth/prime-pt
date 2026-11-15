/// PT的独立导引调度；SBT只含几何占位和精确Guide材质命中组。
module guides;
import mod_guides;
import mod_scene;
import pt.key_guides;
import pt.denoise;
import geometry.camera;
import params;
import primary_cache;

func write_guides<let Cached : bool>()->void
{
    Params p = parameters[0];
    uint2 pixel = DispatchRaysIndex().xy;
    let film =
        geometry::camera_sample_uv_offset(pixel, p.inverse_extent, 0.5 + p.reconstruction.xy);
    float3 direction =
        geometry::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
    GuideTrace trace;
    pt::KeyGuideVertex first;
    bool found;
    if (Cached)
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
    let current = p.current_camera;
    pt::DenoiseCamera previous = { p.previous_position, p.previous_forward, p.previous_right,
                                   p.previous_up };
    Guides guides = { pixel, film, current, previous };
    if (found)
        guides.write_key(trace, first, p.origin.xyz, direction, p.initial_medium, Cached);
    else
        guides.sky(direction);
}

// 生产独立导引总在已完成的共享首视线之后运行。
[shader("raygeneration")]
func guide_main()->void
{
    write_guides<true>();
}

// 独立求交参考不混入缓存首面入口的可达图。
[shader("raygeneration")]
func guide_reference()->void
{
    write_guides<false>();
}
