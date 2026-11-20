//! 用独立的 f64 针孔投影、镜面展开和斯涅尔定律校验写入 SDK 纹理的像素位移。
use super::*;

#[derive(Clone, Copy)]
enum Target {
    Plane { air: f64, glass: f64 },
    MappedMirror,
    Sky,
}

/// 平行界面保持切向波矢。以空气中射线的斜率为自变量，计算接收面的切向位移。
fn travel(slope: f64, air: f64, glass: f64) -> f64 {
    let ior = 1.5;
    air * slope + glass * slope / (ior * ior + (ior * ior - 1.) * slope * slope).sqrt()
}

fn reference(
    target: Target,
    film: [f64; 2],
    extent: [u32; 2],
    shift: [f64; 2],
    yaw: [f64; 2],
    half_width: [f64; 2],
) -> [f64; 2] {
    let [w, h] = extent.map(f64::from);
    let focal = 0.5 * w / half_width[1];
    let x = (film[0] - 0.5 * w) / focal;
    let y = (0.5 * h - film[1]) / focal;
    let (s, c) = yaw[1].sin_cos();
    let z = c - s * x;
    let mut slope = [(s + c * x) / z, y / z];
    if matches!(target, Target::MappedMirror) {
        // 平面 z=2 的光学法线向 +x 倾斜，反射接收面在 z=-1；命中点沿几何平面移动。
        let normal_x = 32.0_f64 / 127.;
        let b = 1. - 2. * normal_x.powi(2);
        let c = 2. * normal_x * (1. - normal_x.powi(2)).sqrt();
        let receiver_x = |x: f64| 2. * x + 3. * (b * x + c) / (b - c * x);
        let point_x = shift[0] + receiver_x(slope[0]);
        let point_y = shift[1] + slope[1] * (2. + 3. / (b - c * slope[0]));
        let (mut lo, mut hi) = (-16., b / c - 1e-6);
        for _ in 0..54 {
            let mid = (lo + hi) * 0.5;
            if receiver_x(mid) < point_x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let x = (lo + hi) * 0.5;
        slope = [x, point_y / (2. + 3. / (b - c * x))];
    }
    if let Target::Plane { air, glass } = target {
        let r = slope[0].hypot(slope[1]);
        let distance = if r == 0. {
            air + glass / 1.5
        } else {
            travel(r, air, glass) / r
        };
        let point = [
            shift[0] + distance * slope[0],
            shift[1] + distance * slope[1],
        ];
        let radius = point[0].hypot(point[1]);
        let mut lo = 0.;
        let mut hi = radius / air;
        for _ in 0..54 {
            let mid = (lo + hi) * 0.5;
            if travel(mid, air, glass) < radius {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        slope = if radius == 0. {
            [0.; 2]
        } else {
            point.map(|v| v * (lo + hi) / (2. * radius))
        };
    }
    let (s, c) = yaw[0].sin_cos();
    let z = s * slope[0] + c;
    let focal = 0.5 * w / half_width[0];
    [
        0.5 * w + focal * (c * slope[0] - s) / z,
        0.5 * h - focal * slope[1] / z,
    ]
}

fn audit_camera(extent: [u32; 2], shift: [f64; 2], yaw: f64, half_width: f64) -> Camera {
    let mut cam = camera();
    let (s, c) = yaw.sin_cos();
    cam.origin = [shift[0], shift[1], 0., 0.].map(|v| v as f32);
    cam.forward = [s, 0., c, 0.].map(|v| v as f32);
    cam.right = [half_width * c, 0., -half_width * s, 0.].map(|v| v as f32);
    cam.up[1] = (half_width * f64::from(extent[1]) / f64::from(extent[0])) as f32;
    cam
}

fn mapped_mirror_depth(pixel_x: f64, width: u32, yaw: f64, half_width: f64) -> f64 {
    let u = (2. * pixel_x / f64::from(width) - 1.) * half_width;
    let (s, c) = yaw.sin_cos();
    let slope = (s + c * u) / (c - s * u);
    let a = 32.0_f64 / 127.;
    let outgoing_z = 1. - 2. * a * a - 2. * a * (1. - a * a).sqrt() * slope;
    (2. + 3. / outgoing_z) * (s * slope + c)
}

#[test]
#[ignore = "真实 DLSS RR，读取两路运动和失配掩码，按输入像素比较全视场的解析对应"]
fn hardware_rr_projection_accuracy() -> Result<(), Box<dyn std::error::Error>> {
    audit(true)
}

#[test]
#[ignore = "真实 NRD，读取两路 2.5D 运动，按输入像素比较全视场的解析对应"]
fn hardware_nrd_projection_accuracy() -> Result<(), Box<dyn std::error::Error>> {
    audit(false)
}

#[test]
#[ignore = "转身后旧相机背后的实体和天空都显露，有限零运动不能复用眼前的颜色"]
fn hardware_rr_previous_camera_back() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [257, 129];
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    materials(gpu, &[emitter([1.; 3])], [0; 4])?;
    gpu.set_scene(&Mesh {
        quads: vec![plane(-12., 12., 3., 0), plane(0., -12., -3., 0)],
    })?;
    let options = Options::new(true, streamline::Quality::Dlaa, 2, 6)?;
    for (i, yaw) in [0., std::f64::consts::PI, std::f64::consts::PI]
        .into_iter()
        .enumerate()
    {
        let cam = audit_camera(extent, [0.; 2], yaw, 0.5);
        host.render_options(gpu, &output, &cam, Environment::default(), options)?;
        if i > 0 {
            let mask = gpu
                .view
                .as_ref()
                .unwrap()
                .rr
                .disocclusion
                .download(&mut gpu.context)?;
            let middle = extent[1] / 2 * extent[0];
            let samples = [extent[0] / 4, 3 * extent[0] / 4].map(|x| mask[(middle + x) as usize]);
            eprintln!("旧相机背后 frame={i} mask={samples:?}");
            assert_eq!(samples, if i == 1 { [255; 2] } else { [0; 2] });
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "玻璃边缘移开时，直视接收面应拒绝旧位置的折射历史"]
fn hardware_rr_leaves_glass() -> Result<(), Box<dyn std::error::Error>> {
    leaves_glass(true)
}

#[test]
#[ignore = "玻璃边缘移开时，NRD 漫反射历史应独立显露"]
fn hardware_nrd_leaves_glass() -> Result<(), Box<dyn std::error::Error>> {
    leaves_glass(false)
}

fn leaves_glass(rr: bool) -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(rr, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [513, 193];
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    let glass = MaterialDefinition {
        base_roughness: [1., 1., 1., 0.],
        control: [1, 3, 0, 0],
        ..Default::default()
    };
    materials(gpu, &[glass, emitter([1.; 3])], [0; 4])?;
    let options = Options::new(rr, streamline::Quality::Dlaa, 2, 8)?;
    let options = if rr { options } else { options.with_nrd() };
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    gpu.set_scene(&Mesh {
        quads: vec![
            plane(-12., 1.85, 2., 0),
            plane(2.4, -12., 3., 0),
            plane(-12., 12., 6., 1),
        ],
    })?;
    let camera = audit_camera(extent, [0.; 2], 0.6f64.asin(), 0.2);
    let mut moved = camera;
    moved.origin[0] = 0.55;
    let center = (extent[1] / 2 * extent[0] + extent[0] / 2) as usize;
    for (frame, cam) in [&camera, &moved, &moved].into_iter().enumerate() {
        host.render_options(gpu, &output, cam, env, options)?;
        if frame == 0 {
            continue;
        }
        let view = gpu.view.as_ref().unwrap();
        let rejected = if rr {
            view.rr.disocclusion.download(&mut gpu.context)?[center] != 0
        } else {
            let mv = float_pixel(&view.rr.images[6].download(&mut gpu.context)?, extent, 4);
            mv[..2] == extent.map(|v| v as f32)
        };
        eprintln!("离开玻璃 rr={rr} frame={frame} rejected={rejected}");
        assert_eq!(rejected, frame == 1);
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 RR/DLAA：比较横移后首帧与同一新视点收敛帧中透过厚玻璃看到的红绿边缘"]
fn hardware_rr_refracted_color_edge() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(true, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [768, 432];
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    let glass = MaterialDefinition {
        base_roughness: [1., 1., 1., 0.],
        control: [1, 3, 0, 0],
        ..Default::default()
    };
    materials(
        gpu,
        &[glass, emitter([1., 0., 0.]), emitter([0., 1., 0.])],
        [0; 4],
    )?;
    gpu.set_scene(&Mesh {
        quads: vec![
            plane(-12., 12., 2., 0),
            plane(12., -12., 3., 0),
            plane(-12., 4.7, 6., 1),
            plane(4.7, 12., 6., 2),
        ],
    })?;
    let options = Options::new(true, streamline::Quality::Dlaa, 2, 8)?;
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let mut cam = audit_camera(extent, [0.; 2], 0.6f64.asin(), 0.18);
    let [w, h] = extent;
    let mut edges = Vec::new();
    for frame in 0..32 {
        if frame == 16 {
            cam.origin[0] = 0.55;
        }
        host.render_options(gpu, &output, &cam, env, options)?;
        if [15, 16, 31].contains(&frame) {
            let view = gpu.view.as_ref().unwrap();
            assert!(view.rr_enabled);
            let image = view.rr.images[6].download(&mut gpu.context)?;
            let mut rows = Vec::new();
            for y in (h / 4..3 * h / 4).step_by(4) {
                // 红绿两侧等强，用有符号色差的零交叉估计亚像素边缘。
                let difference = |x| {
                    let index = ((y * w + x) * 4) as usize;
                    float_component(&image, index + 1, 2) - float_component(&image, index, 2)
                };
                let first = (w / 8..7 * w / 8)
                    .find(|&x| difference(x) >= 0. && difference(x - 1) < 0.)
                    .expect("可见的红绿边缘");
                let left = difference(first - 1);
                let right = difference(first);
                rows.push(first as f32 - 0.5 - left / (right - left));
            }
            edges.push(rows);
        }
    }
    let mut residual: Vec<_> = edges[1]
        .iter()
        .zip(&edges[2])
        .map(|(a, b)| (a - b).abs())
        .collect();
    residual.sort_unstable_by(f32::total_cmp);
    let p95 = residual[residual.len() * 95 / 100];
    let travel = (edges[0][0] - edges[2][0]).abs();
    eprintln!(
        "RR 最终 HDR 红绿边缘 displacement={travel:.3} px first_vs_settled_p95={p95:.3} px max={:.3} px",
        residual.last().unwrap()
    );
    assert!(travel > 50. && p95 <= 1.);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn audit(rr: bool) -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(rr, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = if std::env::var_os("PRIME_AUDIT_8K").is_some() {
        [7680, 320]
    } else if std::env::var_os("PRIME_NRD_4K").is_some() {
        [3840, 2160]
    } else {
        [768, 432]
    };
    let [w, h] = extent;
    let output = Image::new(&mut gpu.context, [w, h, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let glass = MaterialDefinition {
        base_roughness: [1., 1., 1., 0.],
        control: [1, 3, 0, 0],
        ..Default::default()
    };
    let thin = MaterialDefinition {
        control: [3, 3, 0, 0],
        ..glass
    };
    let mirror = MaterialDefinition {
        base_roughness: [0.8, 0.7, 0.5, 0.],
        surface: [1.5, 1., 0., 0.],
        control: [0, 3, 0, 0],
        ..Default::default()
    };
    let mapped_mirror = MaterialDefinition {
        control: [
            0,
            3,
            MaterialDefinition::AUXILIARY,
            MaterialDefinition::AUTHORED_NORMAL,
        ],
        ..mirror
    };
    materials(
        gpu,
        &[glass, thin, mirror, emitter([1.; 3]), mapped_mirror],
        [160, 128, 0, 85],
    )?;
    let options = Options::new(rr, streamline::Quality::Dlaa, 2, 8)?;
    let options = if rr { options } else { options.with_nrd() };
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let receiver = Target::Plane { air: 6., glass: 0. };
    let reflected = Target::Plane { air: 5., glass: 0. };
    let slab = Target::Plane { air: 5., glass: 1. };
    let mirrored_slab = Target::Plane { air: 6., glass: 1. };
    let surface = |z, id| plane(-20., 20., z, id);
    let back = |z, id| plane(20., -20., z, id);
    let tilted = Quad::with_material(
        surface(2., 4).positions(),
        [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
        [1.; 3],
        prime_scene::MaterialId::new(4),
    );
    let cases = [
        ("接收面", vec![surface(6., 3)], [Some(receiver), None]),
        (
            "法线贴图镜面",
            vec![tilted, back(-1., 3)],
            [
                Some(if rr {
                    Target::MappedMirror
                } else {
                    Target::Plane { air: 2., glass: 0. }
                }),
                Some(Target::MappedMirror),
            ],
        ),
        (
            "薄玻璃",
            vec![surface(2., 1), surface(6., 3), back(-1., 3)],
            [Some(receiver), Some(reflected)],
        ),
        (
            "厚玻璃",
            vec![surface(2., 0), back(3., 0), surface(6., 3), back(-1., 3)],
            [Some(slab), Some(reflected)],
        ),
        // RR 在理想金属上替换主表面；NRD 的进入表面分量停在金属，反射分量穿过镜中玻璃。
        (
            "镜中厚玻璃",
            vec![
                surface(2., 2),
                back(-0.5, 0),
                surface(-1.5, 0),
                back(-3., 3),
            ],
            [
                Some(if rr {
                    mirrored_slab
                } else {
                    Target::Plane { air: 2., glass: 0. }
                }),
                Some(mirrored_slab),
            ],
        ),
        (
            "玻璃后天空",
            vec![surface(2., 0), back(3., 0)],
            [Some(Target::Sky); 2],
        ),
        // 直视天空不进入 REBLUR；RR 仍重建天空颜色。
        (
            "直视天空",
            vec![back(-1., 3)],
            [rr.then_some(Target::Sky); 2],
        ),
    ];
    let mut passed = true;
    let reverse_sky = (
        "背向玻璃天空",
        vec![back(-2., 0), surface(-3., 0)],
        [Some(Target::Sky); 2],
    );
    for ((name, quads, targets), reverse) in cases
        .into_iter()
        .map(|case| (case, false))
        .chain([(reverse_sky, true)])
    {
        gpu.set_scene(&Mesh { quads })?;
        for shift in [[0., 0.], [0.44, 0.07], [-3.95, -0.052]] {
            let repeated = shift == [0., 0.];
            let yaw = [0.10, if repeated { 0.10 } else { 0.16 }];
            let half_width = [0.62, if repeated { 0.62 } else { 0.58 }];
            let rotation = if reverse { std::f64::consts::PI } else { 0. };
            let world_shift = [if reverse { -shift[0] } else { shift[0] }, shift[1]];
            let previous = audit_camera(extent, [0.; 2], yaw[0] + rotation, half_width[0]);
            let current = audit_camera(extent, world_shift, yaw[1] + rotation, half_width[1]);
            host.render_options(gpu, &output, &previous, dark, options)?;
            host.render_options(gpu, &output, &current, dark, options)?;
            let view = gpu.view.as_ref().unwrap();
            assert_eq!(view.rr_enabled, rr);
            let jitter = view.jitter.map(f64::from);
            let mask = if rr {
                view.rr.disocclusion.download(&mut gpu.context)?
            } else {
                Vec::new()
            };
            let indices = if rr { [2, 8] } else { [6, 0] };
            for (lane, (image, target)) in indices.into_iter().zip(targets).enumerate() {
                let Some(target) = target else { continue };
                let motion = view.rr.images[image].download(&mut gpu.context)?;
                let components = if rr { 2 } else { 4 };
                let stride = motion.len() / (w * h) as usize;
                let mut errors = Vec::new();
                let mut rejected = 0;
                let mut peak = 0.0_f64;
                let mut max_depth_error = 0.0_f64;
                for y in (3..h - 3).step_by((h / 19).max(1) as usize) {
                    for x in (3..w - 3).step_by((w / 67).max(1) as usize) {
                        let film = [
                            f64::from(x) + 0.5 + jitter[0],
                            f64::from(y) + 0.5 + jitter[1],
                        ];
                        let prev = reference(target, film, extent, shift, yaw, half_width);
                        if !(3.0..f64::from(w) - 3.).contains(&prev[0])
                            || !(3.0..f64::from(h) - 3.).contains(&prev[1])
                        {
                            continue;
                        }
                        let index = (y * w + x) as usize;
                        let mv = [0, 1].map(|c| {
                            f64::from(float_component(
                                &motion,
                                index * components + c,
                                stride / components,
                            ))
                        });
                        if if rr {
                            mask[index] != 0
                        } else {
                            mv == [f64::from(w), f64::from(h)]
                        } {
                            rejected += 1;
                            continue;
                        }
                        let expected = [prev[0] - film[0], prev[1] - film[1]];
                        if !rr && matches!(target, Target::MappedMirror) {
                            let previous_z = mapped_mirror_depth(prev[0], w, yaw[0], half_width[0]);
                            let current_z = mapped_mirror_depth(film[0], w, yaw[1], half_width[1]);
                            let dz = f64::from(float_component(
                                &motion,
                                index * components + 2,
                                stride / components,
                            ));
                            max_depth_error =
                                max_depth_error.max((dz - (previous_z - current_z)).abs());
                        }
                        peak = peak.max(expected[0].abs().max(expected[1].abs()));
                        errors.push((mv[0] - expected[0]).hypot(mv[1] - expected[1]));
                    }
                }
                if errors.is_empty() && rejected == 0 {
                    continue;
                }
                errors.sort_unstable_by(f64::total_cmp);
                let count = errors.len();
                let max = errors.last().copied().unwrap_or(f64::INFINITY);
                let p99 = errors
                    .get(count * 99 / 100)
                    .copied()
                    .unwrap_or(f64::INFINITY);
                let over = errors.iter().filter(|e| **e > 1.).count();
                eprintln!(
                    "重投影 rr={rr} {w}x{h} {name} lane={lane} dx={} accepted={count} rejected={rejected} peak={peak:.2} p99={p99:.4} max={max:.4} over1={over}",
                    shift[0]
                );
                if !rr && matches!(target, Target::MappedMirror) {
                    eprintln!("法线贴图镜面 NRD previous view-Z max_error={max_depth_error:.6} 格");
                }
                passed &=
                    count > 0 && rejected <= count / 4 && max <= 1. && max_depth_error < 0.001;
            }
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    assert!(
        passed,
        "旧帧可见样本的实际纹理运动偏离解析对应或有效覆盖不足"
    );
    Ok(())
}
