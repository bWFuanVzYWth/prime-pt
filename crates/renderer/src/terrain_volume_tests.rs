//! 通过生产主射线、极线体积和合成程序检查旋转与深度断点。
use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn yaw(angle: f32) -> Camera {
    let (s, c) = angle.sin_cos();
    Camera {
        forward: [s, 0., c, 0.],
        right: [c, 0., -s, 0.],
        ..camera()
    }
}

fn frame(host: &Host, gpu: &mut Gpu, target: &Image, camera: &Camera) -> TestResult<Vec<[f32; 3]>> {
    gpu.restart_accumulation()?;
    gpu.absolute_sample = 0;
    gpu.test_center_film = true;
    host.render_options(
        gpu,
        target,
        camera,
        Environment {
            sun_direction: [0., 0.6, -0.8],
            ..Default::default()
        },
        Options::new(false, streamline::Quality::Dlaa, 1, 1)?,
    )?;
    Ok(accumulated_rgb(gpu)?)
}

fn black_plane(z: f32, left: f32, bottom: f32, right: f32, top: f32) -> Quad {
    Quad::new(
        [
            [left, bottom, z],
            [right, bottom, z],
            [right, top, z],
            [left, top, z],
        ],
        [[0.5; 2]; 4],
        [0.; 3],
        Material::Opaque,
    )
}

fn occluded_wall_scene() -> Mesh {
    Mesh {
        quads: vec![
            black_plane(480., -1400., -900., 1400., 900.),
            black_plane(50., -220., -100., -5., 140.),
        ],
    }
}

#[test]
#[ignore = "真实 Vulkan；同一视角的空气散射不应依赖转向前的相机"]
fn terrain_volume_yaw_uses_current_view() -> TestResult {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let target = image(gpu)?;
    exposure_view(gpu)?;
    gpu.set_scene(&occluded_wall_scene())?;
    frame(&host, gpu, &target, &yaw(-0.7))?;
    let turned = frame(&host, gpu, &target, &yaw(0.35))?;
    gpu.terrain_light.reset_history();
    let fresh = frame(&host, gpu, &target, &yaw(0.35))?;
    let mut peak = 0f32;
    let mut energy = [0f64; 2];
    for (a, b) in turned.iter().zip(&fresh) {
        let error = (a[1] - b[1]).abs();
        peak = peak.max(error);
        energy[0] += f64::from(error).powi(2);
        energy[1] += f64::from(b[1]).powi(2);
    }
    let relative = (energy[0] / energy[1]).sqrt();
    eprintln!("转向后与当前视角冷启动对照，绿色通道相对 L2 {relative:.6}，最大绝对差 {peak:.6}");
    assert!(
        relative < 0.025,
        "水平旋转遗留了旧视角的体积，误差 {relative}"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；窄方块拥有与同深度平面相同的受光空气前缀"]
fn terrain_volume_far_block_keeps_air_in_front() -> TestResult {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let target = image(gpu)?;
    exposure_view(gpu)?;
    let view = gpu.view.as_mut().unwrap();
    view.prefix = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    let pixel = 18 * 32 + 26;
    let mut air = [0.; 2];
    for (result, half) in air.iter_mut().zip([600., 0.5]) {
        let (x, y, z) = (131.25, -31.25, 200.);
        gpu.set_scene(&Mesh {
            quads: vec![black_plane(z, x - half, y - half, x + half, y + half)],
        })?;
        gpu.terrain_light.reset_history();
        *result = frame(&host, gpu, &target, &camera())?[pixel][1];
        let mut depth = [0; 4];
        gpu.view.as_ref().unwrap().prefix.read(
            &mut gpu.context,
            (pixel * 16 + 12) as u64,
            &mut depth,
        )?;
        let distance = f32::from_le_bytes(depth);
        assert!(
            (distance - x.hypot(y).hypot(z)).abs() < 0.01,
            "主射线需实际命中方块，距离 {distance}"
        );
    }
    eprintln!(
        "同一主射线的绿色辐亮度，大平面 {:.7}，一米方块 {:.7}",
        air[0], air[1]
    );
    assert!(
        air[0] > 0. && (air[0] - air[1]).abs() < 0.05 * air[0],
        "窄方块前的空气丢失 {air:?}"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "原生 4K，记录有近远遮挡时连续水平旋转的整帧 GPU 时间"]
fn terrain_volume_yaw_4k_timing() -> TestResult {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let target = Image::new(&mut gpu.context, [3840, 2160, 1], ImageFormat::Rgba8Unorm)?;
    let queries = [
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
    ];
    gpu.test_roots.push(target.into());
    gpu.test_roots
        .extend(queries.map(vulkan::native::Resource::from));
    gpu.set_scene(&occluded_wall_scene())?;
    let environment = Environment {
        sun_direction: [0., 0.6, -0.8],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 1)?;
    let mut times = Vec::new();
    for i in 0..25 {
        let mut camera = yaw(0.012 * i as f32 - 0.15);
        for v in &mut camera.right[..3] {
            *v *= 16. / 9.;
        }
        let [_, ns] =
            host.render_profiled(gpu, &target, &camera, environment, options, Some(queries))?;
        if i >= 8 {
            times.push(ns as f64 * 1e-6);
        }
    }
    eprintln!("4K 水平转向整帧 GPU 毫秒 {times:?}");
    times.sort_by(f64::total_cmp);
    eprintln!("4K 水平转向 GPU 中位数 {:.3} ms", times[times.len() / 2]);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
