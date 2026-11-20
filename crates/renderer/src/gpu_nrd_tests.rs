//! 通过两种实际追踪入口检查透明接收面及几何边缘的太阳信号。
use super::*;
use vulkan::nrd::SIGNAL_BYTES_PER_PIXEL;

fn materials<const N: usize>(
    gpu: &mut Gpu,
    definitions: &[MaterialDefinition; N],
    auxiliary: [u8; 4],
) -> Result<(), Error> {
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [1, 1],
        albedo: &[255; 4],
        materials: definitions,
        mers: &[0; 4],
        auxiliary: &auxiliary,
        emission_alias_words: &[],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; N],
    })
}

fn emitter(rgb: [f32; 3]) -> MaterialDefinition {
    MaterialDefinition {
        base_roughness: [0., 0., 0., 1.],
        surface: [1., 0., 0., 0.],
        control: [0, 3, 0, 0],
        emission: [rgb[0], rgb[1], rgb[2], 1.],
        ..Default::default()
    }
}

fn plane(left: f32, right: f32, z: f32, id: u32) -> Quad {
    Quad::with_material(
        [
            [left, -12., z],
            [left, 12., z],
            [right, 12., z],
            [right, -12., z],
        ],
        [[0.5; 2]; 4],
        [1.; 3],
        prime_scene::MaterialId::new(id),
    )
}

fn surface_pixel(gpu: &mut Gpu, capture: Buffer, offset: u64) -> Result<[f32; 4], Error> {
    linear_pixel(
        &mut gpu.context,
        gpu.view.as_ref().unwrap().output,
        capture,
        offset,
    )
}
fn linear_pixel(
    context: &mut Context,
    source: Buffer,
    capture: Buffer,
    offset: u64,
) -> Result<[f32; 4], Error> {
    source.copy_prefix_to(context, &capture, offset + 16)?;
    let mut pixel = [0.0_f32; 4];
    capture.read(context, offset, bytemuck::cast_slice_mut(&mut pixel))?;
    Ok(pixel)
}

#[test]
#[ignore = "真实 Vulkan：透明薄片使用接收面的运动与漫反射历史，并保留玻璃自身的高光粗糙度"]
fn hardware_nrd_glass_receiver() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let native_4k = std::env::var_os("PRIME_NRD_4K").is_some();
    let extent = if native_4k { [3840, 2160] } else { [128, 96] };
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    let mut glass = MaterialDefinition::default();
    glass.base_roughness[3] = 0.;
    glass.control = [3, 3, 0, 0];
    let receiver = MaterialDefinition {
        control: [0, 3, 0, 0],
        ..Default::default()
    };
    materials(gpu, &[receiver, glass], [0; 4])?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 6., [0.5; 2], [0.75, 0.4, 0.2], 12.);
    mesh.quads.push(Quad::with_material(
        [
            [-0.9, -0.9, 2.],
            [-0.9, 0.9, 2.],
            [0.9, 0.9, 2.],
            [0.9, -0.9, 2.],
        ],
        [[0.5; 2]; 4],
        [1.; 3],
        prime_scene::MaterialId::new(1),
    ));
    gpu.set_scene(&mesh)?;
    let mut camera = camera();
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    let env = Environment {
        sun_direction: [0.8, 0., -0.6],
        ..Default::default()
    };
    if native_4k {
        let queries = [
            vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
            vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        ];
        gpu.test_roots
            .extend(queries.map(vulkan::native::Resource::from));
        let mut times = Vec::new();
        for i in 0..15 {
            let [_, ns] =
                host.render_profiled(gpu, &output, &camera, env, options, Some(queries))?;
            if i >= 4 {
                times.push(ns as f64 * 1e-6);
            }
        }
        times.sort_by(f64::total_cmp);
        assert!(times.iter().all(|v| v.is_finite() && *v > 0.));
        let depth = gpu.view.as_ref().unwrap().rr.images[1].download(&mut gpu.context)?;
        let at = ((extent[1] / 2 * extent[0] + extent[0] / 2) * 4) as usize;
        assert!((f32::from_le_bytes(depth[at..at + 4].try_into()?) - 6.).abs() < 0.002);
        eprintln!(
            "NRD glass native=3840x2160 spp=2 depth=6 gpu_median_ms={:.3}",
            times[times.len() / 2]
        );
        session.finish()?;
        assert_eq!(host.errors.load(Ordering::Relaxed), 0);
        return Ok(());
    }
    let mut history = Vec::new();
    for i in 0..49 {
        if i == 48 {
            camera.origin[0] = 0.1875;
        }
        host.render_options(gpu, &output, &camera, env, options)?;
        let view = gpu.view.as_ref().unwrap();
        if i == 0 || i == 48 {
            let depth = view.rr.images[1].download(&mut gpu.context)?;
            let at = (48 * 128 + 64) * 4;
            let z = f32::from_le_bytes(depth[at..at + 4].try_into()?);
            let n = half_pixel(&view.rr.images[3].download(&mut gpu.context)?, extent, 4);
            let mv = half_pixel(&view.rr.images[6].download(&mut gpu.context)?, extent, 4);
            eprintln!("glass receiver depth={z} roughness={} motion={mv:?}", n[3]);
            let spec = half_pixel(&view.rr.images[5].download(&mut gpu.context)?, extent, 4);
            assert!((z - 6.).abs() < 0.001 && (n[3] - 0.9).abs() < 0.001 && spec[3] == 0.);
            if i == 48 {
                assert!((mv[0] - 2.).abs() < 0.01 && mv[1].abs() < 0.01);
            }
        }
        if (16..48).contains(&i) {
            let mut sample = [0.; 2];
            for (v, image) in sample
                .iter_mut()
                .zip(view.nrd.as_ref().unwrap().diffuse_pair())
            {
                *v = f64::from(half_pixel(&image.download(&mut gpu.context)?, extent, 4)[0]);
            }
            history.push(sample);
        }
    }
    let variance = |k: usize| {
        let mean = history.iter().map(|v| v[k]).sum::<f64>() / history.len() as f64;
        history.iter().map(|v| (v[k] - mean).powi(2)).sum::<f64>() / history.len() as f64
    };
    let (raw, denoised) = (variance(0), variance(1));
    let means = [0, 1].map(|k| history.iter().map(|v| v[k]).sum::<f64>() / history.len() as f64);
    eprintln!(
        "glass receiver means={means:?} raw_variance={raw:.8} filtered_variance={denoised:.8}"
    );
    assert!(means[1] > 0. && raw > 0. && denoised < raw);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan：透过玻璃移动时，虚拟背景重投影并保留已降噪的反射"]
fn hardware_nrd_glass_disocclusion() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [128u32, 96];
    let output = Image::new(&mut gpu.context, [128, 96, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut glass = MaterialDefinition::default();
    glass.base_roughness[3] = 0.;
    glass.control = [3, 3, 0, 0];
    materials(
        gpu,
        &[
            glass,
            emitter([1., 0., 0.]),
            emitter([0., 1., 0.]),
            emitter([0., 0., 1.]),
        ],
        [0; 4],
    )?;
    gpu.set_scene(&Mesh {
        quads: vec![
            plane(-12., 12., 2., 0),
            plane(-12., 0., 4., 1),
            plane(-12., 12., 8., 2),
            plane(-12., 12., -3., 3),
        ],
    })?;
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    let mut camera = camera();
    camera.origin[0] = -0.8;
    let at = (extent[1] / 2 * extent[0] + extent[0] / 2) * 16;
    let capture = Buffer::new(&mut gpu.context, u64::from(at + 16), true)?;
    gpu.test_roots.push(capture.into());
    let lane_offset = u64::from(at / 16) * SIGNAL_BYTES_PER_PIXEL;
    let lanes = Buffer::new(&mut gpu.context, lane_offset + SIGNAL_BYTES_PER_PIXEL, true)?;
    gpu.test_roots.push(lanes.into());
    for i in 0..18 {
        if i == 17 {
            camera.origin[0] = 0.4;
        }
        host.render_options(gpu, &output, &camera, env, options)?;
        if i >= 16 {
            let rgba = surface_pixel(gpu, capture, u64::from(at))?;
            let (on, off) = if i == 16 { (0, 1) } else { (1, 0) };
            eprintln!("glass disocclusion frame={i} radiance={rgba:?}");
            // n=1.5 的薄片，两界面的法向反射率 2F/(1+F)，其余能量透射。
            assert!((rgba[on] - 0.92308).abs() < 0.008 && rgba[off] < 0.002);
            assert!((rgba[2] - 0.07692).abs() < 0.007);
            let rr = &gpu.view.as_ref().unwrap().rr.images;
            let z_at = |context: &mut Context, image: Image| -> Result<f32, Error> {
                let bytes = image.download(context)?;
                let j = at as usize / 4;
                Ok(f32::from_le_bytes(bytes[j..j + 4].try_into().unwrap()))
            };
            let dz = z_at(&mut gpu.context, rr[1])?;
            let rz = z_at(&mut gpu.context, rr[7])?;
            let dm = half_pixel(&rr[6].download(&mut gpu.context)?, extent, 4);
            let rm = half_pixel(&rr[0].download(&mut gpu.context)?, extent, 4);
            eprintln!("分路深度 D={dz} R={rz} 运动 D={} R={}", dm[0], rm[0]);
            assert!((dz - if i == 16 { 4. } else { 8. }).abs() < 0.001 && (rz - 7.).abs() < 0.001);
            if i == 17 {
                assert!(
                    (dm[0] - 64. * 1.2 / 8.).abs() < 0.01 && (rm[0] - 64. * 1.2 / 7.).abs() < 0.01
                );
            }
            // 两条实际路径各承载一个事件。固定光路的自发光已完整加回，滤波器只接收命中距离。
            gpu.view.as_ref().unwrap().signal_lanes.copy_prefix_to(
                &mut gpu.context,
                &lanes,
                lane_offset + SIGNAL_BYTES_PER_PIXEL,
            )?;
            let mut words = [[0u16; SIGNAL_BYTES_PER_PIXEL as usize / 4]; 2];
            lanes.read(
                &mut gpu.context,
                lane_offset,
                bytemuck::cast_slice_mut(&mut words),
            )?;
            let mut samples = [0; 2];
            for lane in words {
                let [diff, spec] = [0, 4].map(|j| [0, 1, 2, 3].map(|c| half(lane[j + c])));
                if diff[3] > 0. {
                    assert_eq!(spec, [0.; 4]);
                    assert_eq!(&diff[..3], &[0.; 3]);
                    samples[0] += 1;
                } else {
                    assert_eq!(diff, [0.; 4]);
                    assert_eq!(&spec[..3], &[0.; 3]);
                    assert!(spec[3] > 0.);
                    samples[1] += 1;
                }
            }
            assert_eq!(samples, [1, 1]);
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "玻璃透射到天空、反射到蓝色发光面；移除两路 REBLUR 输出应逐路移除最终辐射"]
fn hardware_nrd_glass_filtered_lobes() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [65, 65];
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    let at = (32 * 65 + 32) * 16;
    let capture = Buffer::new(&mut gpu.context, at + 16, true)?;
    gpu.test_roots.push(output.into());
    gpu.test_roots.push(capture.into());
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    // 用无吸收的外部介质隔离相机段空气散射。
    gpu.set_initial_medium([1.1, 0., 0., 0.])?;
    for roughness in [0., 0.25] {
        let mut glass = MaterialDefinition::default();
        glass.base_roughness[3] = roughness;
        glass.control = [3, 3, 0, 0];
        materials(gpu, &[glass, emitter([0., 0., 8.])], [0; 4])?;
        gpu.set_scene(&Mesh {
            quads: vec![plane(-12., 12., 2., 0), plane(-12., 12., -3., 1)],
        })?;
        let mut camera = camera();
        for i in 0..12 {
            if i == 11 {
                camera.origin[0] = 0.125;
            }
            host.render_options(gpu, &output, &camera, Environment::default(), options)?;
        }
        let view = gpu.view.as_ref().unwrap();
        let normal = half_pixel(&view.rr.images[3].download(&mut gpu.context)?, extent, 4);
        let depth = view.rr.images[1].download(&mut gpu.context)?;
        let zi = (32 * 65 + 32) * 4;
        let z = f32::from_le_bytes(depth[zi..zi + 4].try_into()?);
        assert!((z - 2.).abs() < 0.001 && (normal[3] - roughness).abs() < 0.001);
        let r_depth = view.rr.images[7].download(&mut gpu.context)?;
        let rz = f32::from_le_bytes(r_depth[zi..zi + 4].try_into()?);
        let d_motion = half_pixel(&view.rr.images[6].download(&mut gpu.context)?, extent, 4);
        let r_motion = half_pixel(&view.rr.images[0].download(&mut gpu.context)?, extent, 4);
        let spec = half_pixel(&view.rr.images[5].download(&mut gpu.context)?, extent, 4);
        assert!((spec[3] - roughness).abs() < 0.001);
        if roughness == 0. {
            assert!(d_motion[0].abs() < 0.001 && (rz - 7.).abs() < 0.001 && r_motion[0] > 0.1);
        } else {
            assert!((rz - z).abs() < 0.001 && d_motion[0] > 0.1 && d_motion[..3] == r_motion[..3]);
        }
        let nrd = view.nrd.as_ref().unwrap();
        let diffuse = nrd.diffuse_pair()[1];
        let specular = nrd.specular_signals()[2];
        let black = vec![0; (extent[0] * extent[1] * 8) as usize];
        let full = surface_pixel(gpu, capture, at)?;
        // 原始路径、天空与材质都保留，逐个清空已滤波的颜色并再次运行生产合成。
        for (i, lobe) in [diffuse, specular].into_iter().enumerate() {
            lobe.upload(&mut gpu.context, &black)?;
            gpu.view
                .as_ref()
                .unwrap()
                .nrd
                .as_ref()
                .unwrap()
                .compose(&mut gpu.context, extent)?
                .submit_and_wait(&mut gpu.context)?;
            let rgb = surface_pixel(gpu, capture, at)?;
            eprintln!(
                "glass roughness={roughness} removed={} full={full:?} remaining={rgb:?}",
                i + 1
            );
            if i == 0 {
                assert!(full[0] > 0.01 && full[1] > 0.01 && full[2] > rgb[2]);
                assert!(rgb[0].abs() < 0.003 && rgb[1].abs() < 0.003 && rgb[2] > 0.1);
            } else if roughness == 0. {
                let f = ((1.5_f32 - 1.1) / (1.5 + 1.1)).powi(2);
                assert!((rgb[2] - 8. * 2. * f / (1. + f)).abs() < 0.003);
                assert_eq!(&rgb[..2], &[0.; 2]);
            } else {
                assert_eq!(&rgb[..3], &[0.; 3]);
            }
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "同一平面相邻纹素的亮度阶跃应从解调信号中消失，并在逐波瓣调制后回到当前像素"]
fn hardware_nrd_delta_texture() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [65, 65];
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    let offset = (32 * 65 + 32) * 16;
    let capture = Buffer::new(&mut gpu.context, offset + 16, true)?;
    gpu.test_roots.push(output.into());
    gpu.test_roots.push(capture.into());
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 8)?.with_nrd();
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let glass = MaterialDefinition {
        base_roughness: [1., 1., 1., 0.],
        control: [3, 3, 0, 0],
        ..Default::default()
    };
    let matte = MaterialDefinition {
        base_roughness: [1.; 4],
        surface: [1., 0., 0., 0.],
        control: [0, 3, 0, 0],
        ..Default::default()
    };
    for self_lit in [true, false] {
        gpu.replace_resources(ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [2, 1],
            albedo: &[96, 96, 96, 255, 255, 255, 255, 255],
            materials: &[
                glass,
                if self_lit { emitter([4.; 3]) } else { matte },
                emitter([4.; 3]),
            ],
            mers: &[0; 8],
            auxiliary: &[0; 8],
            emission_alias_words: &[],
            opacity_quads: &[],
            light_areas: &[[u32::MAX, 0, 0, 0]; 3],
        })?;
        let patch = |lo, hi, z, id, u| {
            Quad::with_material(
                plane(lo, hi, z, id).positions(),
                [[u, 0.5]; 4],
                [1.; 3],
                prime_scene::MaterialId::new(id),
            )
        };
        gpu.set_scene(&Mesh {
            quads: vec![
                patch(-150., 150., 2., 0, 0.75),
                patch(-150., 0., 6., 1, 0.25),
                patch(0., 150., 6., 1, 0.75),
                patch(-150., 0., -3., 2, 0.75),
                patch(0., 150., -3., 2, if self_lit { 0.25 } else { 0.75 }),
            ],
        })?;
        let mut camera = camera();
        let mut samples = Vec::new();
        for i in 0..42 {
            camera.origin[0] = if i < 40 { -0.04 } else { 0.04 };
            host.render_options(gpu, &output, &camera, env, options)?;
            if i < 39 {
                continue;
            }
            let view = gpu.view.as_ref().unwrap();
            let nrd = view.nrd.as_ref().unwrap();
            let mut values = Vec::new();
            for img in [
                view.rr.images[4],
                view.rr.images[5],
                nrd.diffuse_pair()[0],
                nrd.diffuse_pair()[1],
                nrd.specular_signals()[0],
                nrd.specular_signals()[2],
            ] {
                values.push(half_pixel(&img.download(&mut gpu.context)?, extent, 4)[0]);
            }
            let emission = linear_pixel(&mut gpu.context, view.prefix, capture, offset)?[0];
            let rgb = surface_pixel(gpu, capture, offset)?;
            eprintln!(
                "texture self_lit={self_lit} frame={i} factors/input/output={values:?} final={}",
                rgb[0]
            );
            assert!(
                (rgb[0] - (emission + values[0] * values[3] + values[1] * values[5])).abs() < 0.008
            );
            if self_lit {
                assert!(values[2..].iter().all(|v| v.abs() < 0.0001));
                let dim = ((96. / 255. + 0.055_f32) / 1.055).powf(2.4);
                let (through, mirror) = if i < 40 { (dim, 1.) } else { (1., dim) };
                assert!((emission - 4. * (12. * through + mirror) / 13.).abs() < 0.002);
            } else {
                assert!((emission - 4. / 13.).abs() < 0.002);
            }
            samples.push(values);
        }
        let (a, b) = (&samples[0], &samples[1]);
        if !self_lit {
            assert!(b[0] > 3. * a[0] && b[3] > 0.);
            assert!((a[3] / b[3] - 1.).abs() < 0.18);
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "分层介电、分数金属和次表面底层的两个首面提案各自只写入自己的波瓣"]
fn hardware_nrd_layered_lobes() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [65, 65];
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    let pixel = 32 * 65 + 32;
    let offset = pixel * SIGNAL_BYTES_PER_PIXEL;
    let capture = Buffer::new(&mut gpu.context, offset + SIGNAL_BYTES_PER_PIXEL, true)?;
    gpu.test_roots.push(output.into());
    gpu.test_roots.push(capture.into());
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 5)?.with_nrd();
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    for (roughness, metal) in [(0., 0.), (0.4, 0.35), (0.4, 1.)] {
        let front = MaterialDefinition {
            base_roughness: [0.7, 0.45, 0.2, roughness],
            surface: [1.5, metal, 0.4, 0.],
            control: [0, 3, 0, 0],
            ..Default::default()
        };
        materials(gpu, &[front, emitter([1.; 3])], [0; 4])?;
        gpu.set_scene(&Mesh {
            quads: vec![
                plane(-100., 100., 2., 0),
                plane(-100., 100., -3., 1),
                plane(-100., 100., 6., 1),
            ],
        })?;
        let mut sampled = [0; 2];
        for _ in 0..24 {
            host.render_options(gpu, &output, &camera(), dark, options)?;
            gpu.view.as_ref().unwrap().signal_lanes.copy_prefix_to(
                &mut gpu.context,
                &capture,
                offset + SIGNAL_BYTES_PER_PIXEL,
            )?;
            let mut words = [[0u16; SIGNAL_BYTES_PER_PIXEL as usize / 4]; 2];
            capture.read(
                &mut gpu.context,
                offset,
                bytemuck::cast_slice_mut(&mut words),
            )?;
            for lane in words {
                let [inside, outside] = [3, 7].map(|j| half(lane[j]) > 0.);
                assert!(!(inside && outside));
                sampled[0] += u32::from(inside);
                sampled[1] += u32::from(outside);
            }
        }
        assert!(if metal == 1. {
            sampled[0] == 0 && sampled[1] > 30
        } else {
            sampled[0] > 16 && sampled[1] > 16
        });
        let rr = &gpu.view.as_ref().unwrap().rr.images;
        let depth = |ctx: &mut Context, img: Image| -> Result<f32, Error> {
            let bytes = img.download(ctx)?;
            let start = (pixel * 4) as usize;
            Ok(f32::from_le_bytes(
                bytes[start..start + 4].try_into().unwrap(),
            ))
        };
        let dz = depth(&mut gpu.context, rr[1])?;
        let rz = depth(&mut gpu.context, rr[7])?;
        let factor = half_pixel(&rr[5].download(&mut gpu.context)?, extent, 4);
        eprintln!(
            "分层材质 roughness={roughness} metal={metal} samples={sampled:?} depth=({dz},{rz}) spec={factor:?}"
        );
        assert!(
            (dz - 2.).abs() < 0.001 && (rz - if roughness == 0. { 7. } else { 2. }).abs() < 0.001
        );
        if metal == 1. {
            assert!(factor[0] > factor[1] && factor[1] > factor[2]);
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "斜视有厚度的玻璃，固定接收面必须来自实际折射射线并保留介质吸收"]
fn hardware_nrd_solid_refraction() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let mut glass = MaterialDefinition::default();
    glass.control = [1, 3, 0, 0];
    glass.base_roughness[3] = 0.;
    glass.extinction = [1.5, 0., 0.5, 0.];
    materials(
        gpu,
        &[glass, emitter([1., 1., 0.]), emitter([0., 0., 1.])],
        [0; 4],
    )?;
    // 出射面翻转绕序。真折射在 x=4.186 命中黄色，直走会在 x=4.5 命中蓝色。
    gpu.set_scene(&Mesh {
        quads: vec![
            plane(-12., 12., 2., 0),
            plane(12., -12., 3., 0),
            plane(-12., 4.35, 6., 1),
            plane(4.35, 12., 6., 2),
        ],
    })?;
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut camera = camera();
    camera.forward = [0.6, 0., 0.8, 0.];
    // 窄视场使整个滤波邻域都看到折射后的黄色目标；直走的相机射线仍命中蓝色。
    camera.right = [0.008, 0., -0.006, 0.];
    camera.up[1] = 0.01;
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 8)?.with_nrd();
    let at = (32 * 65 + 32) * 16;
    let capture = Buffer::new(&mut gpu.context, at + 16, true)?;
    gpu.test_roots.push(capture.into());
    let mut radiance = [0.0_f32; 4];
    for i in 0..64 {
        host.render_options(gpu, &output, &camera, dark, options)?;
        if i < 16 {
            continue;
        }
        let pixel = surface_pixel(gpu, capture, at)?;
        for (sum, value) in radiance.iter_mut().zip(pixel) {
            *sum += value / 48.;
        }
    }
    let depth = gpu.view.as_ref().unwrap().rr.images[1].download(&mut gpu.context)?;
    let zi = (32 * 65 + 32) * 4;
    let z = f32::from_le_bytes(depth[zi..zi + 4].try_into()?);
    let transmittance = (-0.5 / (1.0_f32 - 0.4 * 0.4).sqrt()).exp();
    eprintln!("solid slab virtual_z={z} radiance={radiance:?} beer={transmittance}");
    assert!((z - 7.5).abs() < 0.002 && (0.89..0.95).contains(&radiance[0]));
    assert!((radiance[1] / radiance[0] - transmittance).abs() < 0.01 && radiance[2] < 0.01);
    // 在狭视场中心独立求解斯涅尔定律的前帧入射角。目标在 z=6，空气中走 5，玻璃中走 1。
    let color = linear_pixel(
        &mut gpu.context,
        gpu.view.as_ref().unwrap().prefix,
        capture,
        at,
    )?;
    assert!((color[1] / color[0] - transmittance).abs() < 0.002);
    camera.origin[0] = 0.01;
    host.render_options(gpu, &output, &camera, dark, options)?;
    let mv = half_pixel(
        &gpu.view.as_ref().unwrap().rr.images[6].download(&mut gpu.context)?,
        [65, 65],
        4,
    );
    let target_x = 0.01 + 5. * 0.75 + 0.4 / (1.0_f64 - 0.4 * 0.4).sqrt();
    let (mut low, mut high) = (0.6f64, 0.61f64);
    for _ in 0..40 {
        let sin_i = (low + high) * 0.5;
        let sin_t = sin_i / 1.5;
        let x = 5. * sin_i / (1. - sin_i * sin_i).sqrt() + sin_t / (1. - sin_t * sin_t).sqrt();
        if x < target_x {
            low = sin_i;
        } else {
            high = sin_i;
        }
    }
    let cos_i = (1. - low * low).sqrt();
    let projected = 3250. * (0.8 * low - 0.6 * cos_i) / (0.6 * low + 0.8 * cos_i);
    eprintln!("玻璃板折射运动 {} px，斯涅尔参考 {projected}", mv[0]);
    assert!((f64::from(mv[0]) - projected).abs() < 0.02 && mv[1].abs() < 0.002);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "斜视光滑金属时固定光路保留当前 Fresnel 和发光纹素的线性强度"]
fn hardware_nrd_mirror_emission() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let color = [0.24, 0.55, 0.8];
    let mirror = MaterialDefinition {
        base_roughness: [color[0], color[1], color[2], 0.],
        surface: [1.5, 1., 0., 0.],
        control: [0, 3, 0, 0],
        ..Default::default()
    };
    materials(gpu, &[mirror, emitter([4.; 3])], [0; 4])?;
    gpu.set_scene(&Mesh {
        quads: vec![plane(-100., 100., 2., 0), plane(-100., 100., -1., 1)],
    })?;
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    let at = (32 * 65 + 32) * 16;
    let pickup = Buffer::new(&mut gpu.context, at + 16, true)?;
    gpu.test_roots
        .extend([vulkan::native::Resource::from(output), pickup.into()]);
    let cos_i = 0.32_f32;
    let sin_i = (1. - cos_i * cos_i).sqrt();
    let mut camera = camera();
    camera.forward = [sin_i, 0., cos_i, 0.];
    camera.right = [cos_i, 0., -sin_i, 0.];
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    host.render_options(
        gpu,
        &output,
        &camera,
        dark,
        Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd(),
    )?;
    let fixed = linear_pixel(
        &mut gpu.context,
        gpu.view.as_ref().unwrap().prefix,
        pickup,
        at,
    )?;
    let final_rgb = surface_pixel(gpu, pickup, at)?;
    for i in 0..3 {
        let expected = 4. * (color[i] + (1. - color[i]) * (1. - cos_i).powi(5));
        assert!(
            (fixed[i] - expected).abs() < 0.001,
            "Fresnel {fixed:?}, expected={expected}"
        );
        assert!((final_rgb[i] - fixed[i]).abs() < 0.0001);
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "斜视厚玻璃横移半格时，用前帧实际光路校验亚像素对应，并拒绝被遮挡的历史"]
fn hardware_nrd_refracted_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let native_4k = std::env::var_os("PRIME_NRD_4K").is_some();
    let extent = if native_4k { [3840, 2160] } else { [257, 65] };
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
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 8)?.with_nrd();
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    // 实体厚 1、空气段合计 5。独立解出前帧能看见同一终点的入射角。
    let dx = 0.55;
    let theta = 0.6f64.asin();
    let scale = f64::from(extent[0]) / 0.2;
    let center = if extent[0] % 2 == 0 { 0.5 } else { 0. };
    let ray_angle = theta + (center / scale).atan();
    let x = |angle: f64| 5. * angle.tan() + (angle.sin() / 1.5).asin().tan();
    let (mut lo, mut hi) = (ray_angle, 1.);
    for _ in 0..50 {
        let mid = (lo + hi) * 0.5;
        if x(mid) < x(ray_angle) + dx {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let expected = (lo - theta).tan() * scale - center;
    for blocked in [false, true] {
        let mut quads = vec![
            plane(-12., 12., 2., 0),
            plane(12., -12., 3., 0),
            plane(-12., 12., 6., 1),
        ];
        if blocked {
            let center = lo.tan() as f32;
            quads.push(plane(center - 0.04, center + 0.04, 1., 1));
        }
        gpu.set_scene(&Mesh { quads })?;
        let mut camera = camera();
        camera.forward = [0.6, 0., 0.8, 0.];
        camera.right = [0.08, 0., -0.06, 0.];
        camera.up[1] = 0.1 * extent[1] as f32 / extent[0] as f32;
        host.render_options(gpu, &output, &camera, dark, options)?;
        camera.origin[0] = dx as f32;
        host.render_options(gpu, &output, &camera, dark, options)?;
        let view = gpu.view.as_ref().unwrap();
        let mv = half_pixel(&view.rr.images[6].download(&mut gpu.context)?, extent, 4);
        eprintln!("折射对应 blocked={blocked} measured={mv:?} reference={expected}");
        if blocked {
            assert!(mv[0] >= extent[0] as f32);
        } else {
            let tolerance = if native_4k { 0.6 } else { 0.1 };
            assert!((f64::from(mv[0]) - expected).abs() < tolerance && mv[1].abs() < 0.1);
            if native_4k {
                let queries = [
                    vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
                    vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
                ];
                gpu.test_roots
                    .extend(queries.map(vulkan::native::Resource::from));
                let mut times = Vec::new();
                for i in 0..15 {
                    camera.origin[0] = if i % 2 == 0 { 0. } else { dx as f32 };
                    let [_, ns] =
                        host.render_profiled(gpu, &output, &camera, dark, options, Some(queries))?;
                    if i >= 4 {
                        times.push(ns as f64 * 1e-6);
                    }
                }
                times.sort_by(f64::total_cmp);
                assert!(times.iter().all(|t| t.is_finite() && *t > 0.));
                eprintln!(
                    "NRD moving solid glass native=3840x2160 spp=2 depth=8 gpu_median_ms={:.3}",
                    times[times.len() / 2]
                );
            }
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "同一个 quad 的发光贴图阶跃；无入射光时相邻非发光纹素的线性辐亮度为零"]
fn hardware_nrd_emissive_texel_edge() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = Image::new(&mut gpu.context, [64, 32, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let pickup = Buffer::new(&mut gpu.context, 64 * 32 * 16, true)?;
    gpu.test_roots.push(pickup.into());
    let lamp = MaterialDefinition {
        base_roughness: [0.7, 0.7, 0.7, 1.],
        surface: [1., 0., 0., 0.],
        emission: [1., 1., 1., 4.],
        control: [
            0,
            3,
            MaterialDefinition::TEXTURED_EMISSION | MaterialDefinition::MERS,
            0,
        ],
        ..Default::default()
    };
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [2, 1],
        albedo: &[255; 8],
        materials: &[lamp],
        mers: &[0, 255, 255, 0, 0, 0, 255, 0],
        auxiliary: &[0; 8],
        emission_alias_words: &[],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]],
    })?;
    gpu.set_scene(&Mesh {
        quads: vec![Quad::with_material(
            plane(-12., 12., 6., 0).positions(),
            [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
            [1.; 3],
            prime_scene::MaterialId::new(0),
        )],
    })?;
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 3)?.with_nrd();
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let mut camera = camera();
    for i in 0..20 {
        if i == 19 {
            camera.origin[0] = 0.375;
        }
        host.render_options(gpu, &output, &camera, dark, options)?;
        if i == 0 || i >= 18 {
            let first_dark = if i == 19 { 30 } else { 32 };
            let left = surface_pixel(gpu, pickup, (16 * 64 + first_dark - 1) * 16)?[0];
            let right = surface_pixel(gpu, pickup, (16 * 64 + first_dark) * 16)?[0];
            eprintln!("发光边缘 frame={i} bright={left} dark={right}");
            assert_eq!([left, right], [4., 0.]);
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "倾斜玻璃两侧均为天空时，横移相机应保持角向运动为零并重投影首个界面的深度"]
fn hardware_nrd_optical_sky() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [65, 65];
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let glass = MaterialDefinition {
        base_roughness: [1., 1., 1., 0.],
        control: [3, 3, 0, 0],
        ..Default::default()
    };
    materials(gpu, &[glass], [0; 4])?;
    let tilted = plane(-12., 12., 2., 0)
        .positions()
        .map(|[x, y, z]| [x, y, z + 0.25 * x]);
    gpu.set_scene(&Mesh {
        quads: vec![Quad::with_material(
            tilted,
            [[0.5; 2]; 4],
            [1.; 3],
            prime_scene::MaterialId::new(0),
        )],
    })?;
    let mut camera = camera();
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    host.render_options(gpu, &output, &camera, Environment::default(), options)?;
    camera.origin[0] = 0.6;
    host.render_options(gpu, &output, &camera, Environment::default(), options)?;
    let rr = &gpu.view.as_ref().unwrap().rr.images;
    for image in [rr[6], rr[0]] {
        let mv = half_pixel(&image.download(&mut gpu.context)?, extent, 4);
        eprintln!("天空运动 xy=({}, {}) dz={}", mv[0], mv[1], mv[2]);
        assert!(mv[0].abs() < 0.001 && mv[1].abs() < 0.001 && (mv[2] + 0.15).abs() < 0.001);
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "由介质内部发生全反射时，虚拟主表面随镜像平面展开"]
fn hardware_nrd_total_internal_reflection() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let mut glass = MaterialDefinition::default();
    glass.base_roughness[3] = 0.;
    glass.control = [1, 3, 0, 0];
    // 红色发光面也在玻璃介质内，匹配其 IOR 才能隔离界面自身的全反射。
    let mut submerged = emitter([1., 0., 0.]);
    submerged.surface[0] = glass.extinction[0];
    materials(gpu, &[glass, submerged, emitter([0., 1., 0.])], [0; 4])?;
    gpu.set_scene(&Mesh {
        quads: vec![
            plane(12., -12., 2., 0),
            plane(-12., 12., -1., 1),
            plane(-12., 12., 3., 2),
        ],
    })?;
    gpu.set_initial_medium([1.5, 0., 0., 0.])?;
    let mut camera = camera();
    camera.forward = [0.8, 0., 0.6, 0.];
    camera.right = [0.6, 0., -0.8, 0.];
    let output = Image::new(&mut gpu.context, [65, 65, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    host.render_options(
        gpu,
        &output,
        &camera,
        Environment {
            sun_direction: [0.; 3],
            ..Default::default()
        },
        options,
    )?;
    let view = gpu.view.as_ref().unwrap();
    let depth = view.rr.images[7].download(&mut gpu.context)?;
    let at = (32 * 65 + 32) * 4;
    let z = f32::from_le_bytes(depth[at..at + 4].try_into()?);
    let offset = (at * 4) as u64;
    let capture = Buffer::new(&mut gpu.context, offset + 16, true)?;
    gpu.test_roots.push(capture.into());
    let rgb = surface_pixel(gpu, capture, offset)?;
    eprintln!("TIR virtual_z={z} radiance={rgb:?}");
    assert!((z - 5. / 0.6).abs() < 0.002 && rgb[0] > 0.95 && rgb[1] < 0.002);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实太阳射线不能被方块边缘的倾斜贴图法线带到几何背面"]
fn hardware_nrd_sun_geometric_edge() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let image = image(gpu)?;
    let material = MaterialDefinition {
        control: [
            0,
            3,
            MaterialDefinition::AUXILIARY,
            MaterialDefinition::AUTHORED_NORMAL,
        ],
        ..Default::default()
    };
    materials(gpu, &[material], [225, 128, 0, 85])?;
    gpu.set_scene(&Mesh {
        quads: vec![Quad::with_material(
            [[-4., -4., 3.], [-4., 4., 3.], [4., 4., 3.], [4., -4., 3.]],
            [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
            [1.; 3],
            prime_scene::MaterialId::new(0),
        )],
    })?;
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 2)?.with_nrd();
    let size = 32 * 32 * SIGNAL_BYTES_PER_PIXEL;
    let readback = Buffer::new(&mut gpu.context, size, true)?;
    gpu.test_roots.push(readback.into());
    for z in [-0.2_f32, 0.2] {
        let env = Environment {
            sun_direction: [0.95_f32.sqrt(), 0.1, z],
            ..Default::default()
        };
        host.render_options(gpu, &image, &camera(), env, options)?;
        let view = gpu.view.as_ref().unwrap();
        let n = half_pixel(&view.rr.images[3].download(&mut gpu.context)?, [32, 32], 4);
        assert!(n[0] > 0.7 && n[2] < -0.5, "边缘有倾斜的贴图法线 {n:?}");
        view.signal_lanes
            .copy_prefix_to(&mut gpu.context, &readback, size)?;
        for lane in 0..2 {
            let mut solar = [0; 8];
            readback.read(
                &mut gpu.context,
                (16 * 32 + 16) * SIGNAL_BYTES_PER_PIXEL + lane * (SIGNAL_BYTES_PER_PIXEL / 2) + 32,
                &mut solar,
            )?;
            let red = half(u16::from_le_bytes(solar[..2].try_into()?));
            let blocker = half(u16::from_le_bytes(solar[6..].try_into()?));
            eprintln!("edge sun.z={z} lane={lane} direct={red} blocker={blocker}");
            if z > 0. {
                assert_eq!([red, blocker], [0., 0.]);
            } else {
                assert!(red > 0. && blocker == 65504.);
            }
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "有限太阳圆盘：贴着遮挡墙的暗像素不得从相邻亮像素漏进直射光"]
fn hardware_nrd_sun_contact_edge() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [128, 64];
    let output = Image::new(&mut gpu.context, [128, 64, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [1.; 3], 12.);
    mesh.quads.push(Quad::new(
        [
            [0., -10., 3.55],
            [0., 10., 3.55],
            [0., 10., 4.],
            [0., -10., 4.],
        ],
        [[0.5; 2]; 4],
        [1.; 3],
        Material::Opaque,
    ));
    gpu.set_scene(&mesh)?;
    let env = Environment {
        sun_direction: [0.8, 0.48, -0.36],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd();
    for _ in 0..16 {
        host.render_options(gpu, &output, &camera(), env, options)?;
    }
    let view = gpu.view.as_ref().unwrap();
    let nrd = view.nrd.as_ref().unwrap();
    let samples = 128u64 * 64;
    let staging = Buffer::new(&mut gpu.context, samples * SIGNAL_BYTES_PER_PIXEL, true)?;
    gpu.test_roots.push(staging.into());
    let read_radiance = |context: &mut Context| -> Result<[f32; 6], Error> {
        view.output
            .copy_prefix_to(context, &staging, samples * 16)?;
        let mut row = [[0.0_f32; 4]; 6];
        staging.read(
            context,
            (32 * 128 + 61) * 16,
            bytemuck::cast_slice_mut(&mut row),
        )?;
        Ok(row.map(|rgb| rgb[0]))
    };
    let lit = read_radiance(&mut gpu.context)?;
    let penumbra = nrd.shadow_pair()[0].download(&mut gpu.context)?;
    let radius = std::array::from_fn::<_, 6, _>(|x| {
        let offset = (32 * 128 + 61 + x) * 4;
        f32::from_le_bytes(penumbra[offset..offset + 4].try_into().unwrap())
    });
    // 固定同一次追踪、同一份 REBLUR/SIGMA 输出，只移除直射的 RGB 后再次合成。
    view.signal_lanes.copy_prefix_to(
        &mut gpu.context,
        &staging,
        samples * SIGNAL_BYTES_PER_PIXEL,
    )?;
    let mut lanes = vec![0u8; (samples * SIGNAL_BYTES_PER_PIXEL) as usize];
    staging.read(&mut gpu.context, 0, &mut lanes)?;
    for lane in lanes.chunks_exact_mut((SIGNAL_BYTES_PER_PIXEL / 2) as usize) {
        lane[32..38].fill(0);
    }
    staging.write(&mut gpu.context, 0, &lanes)?;
    staging.copy_prefix_to(
        &mut gpu.context,
        &view.signal_lanes,
        samples * SIGNAL_BYTES_PER_PIXEL,
    )?;
    nrd.compose(&mut gpu.context, extent)?
        .submit_and_wait(&mut gpu.context)?;
    let indirect = read_radiance(&mut gpu.context)?;
    let direct = std::array::from_fn::<_, 6, _>(|x| lit[x] - indirect[x]);
    eprintln!("contact edge raw_radius={radius:?} direct_red={direct:?}");
    assert!(radius[..3].iter().all(|v| *v > 0. && *v < 0.01));
    assert!(radius[3..].iter().all(|v| *v >= 65504.));
    assert!(direct[..3].iter().all(|v| v.abs() < 1e-5));
    assert!(direct[3..].iter().all(|v| *v > 0.01));
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
