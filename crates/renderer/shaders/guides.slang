/// PT的独立导引调度；SBT只含几何占位和精确Guide材质命中组。
module guides;
import mod_guides;
import mod_scene;
import pt.key_guides;
import camera.pinhole;
import params;
import primary_cache;

func write_guides<let cached : bool>()->void
{
    let p : Params = parameters[0];
    let pixel : uint2 = DispatchRaysIndex().xy;
    let film = camera::camera_sample_uv_offset(pixel, p.inverse_extent, 0.5 + p.reconstruction.xy);
    var direction : float3 =
                        camera::camera_scaled_direction(p.forward.xyz, p.right.xyz, p.up.xyz, film);
    var trace : GuideTrace;
    var first : pt::KeyGuideVertex;
    var found : bool;
    if (cached)
    {
        let index : uint = primary_pixel();
        let primitive : uint = primary_identity[index].x;
        direction = primary_ray[index].xyz;
        found = primitive != 0xffffffffu;
        first = {};
        if (found)
        {
            let program : uint = guide_slot(primitive);
            var call : pt::KeyGuideCall = {};
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
    let previous : camera::Camera = { p.previous_position, p.previous_forward, p.previous_right,
                                      p.previous_up };
    var guides : Guides = { pixel, film, current, previous };
    if (found)
        guides.write_key(trace, first, p.origin.xyz, direction, p.initial_medium, cached);
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
