//! 通过两种实际追踪入口检查透明接收面及几何边缘的太阳信号。
use super::*;

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
    let view = gpu.view.as_ref().unwrap();
    view.output
        .copy_prefix_to(&mut gpu.context, &capture, offset + 16)?;
    let mut pixel = [0.0_f32; 4];
    capture.read(
        &mut gpu.context,
        offset,
        bytemuck::cast_slice_mut(&mut pixel),
    )?;
    Ok(pixel)
}

#[test]
#[ignore = "真实 Vulkan：透明薄片后的接收面应拥有自己的材质、运动和 REBLUR 历史"]
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
            assert!((z - 6.).abs() < 0.001 && (n[3] - 0.9).abs() < 0.001);
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
#[ignore = "真实 Vulkan：透过玻璃移动时，当前背景的边界与独立 Fresnel 反射应同时保留"]
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
            plane(-12., 0., 6., 1),
            plane(0., 12., 6., 2),
            plane(-12., 12., -3., 3),
        ],
    })?;
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    let mut camera = camera();
    camera.origin[0] = -0.25;
    let at = (extent[1] / 2 * extent[0] + extent[0] / 2) * 16;
    let capture = Buffer::new(&mut gpu.context, u64::from(at + 16), true)?;
    gpu.test_roots.push(capture.into());
    for i in 0..18 {
        if i == 17 {
            camera.origin[0] = 0.25;
        }
        host.render_options(gpu, &output, &camera, env, options)?;
        if i >= 16 {
            let rgba = surface_pixel(gpu, capture, u64::from(at))?;
            let (on, off) = if i == 16 { (0, 1) } else { (1, 0) };
            eprintln!("glass disocclusion frame={i} radiance={rgba:?}");
            // n=1.5 的薄片，两界面的法向反射率 2F/(1+F)，其余能量透射。
            assert!((rgba[on] - 0.92308).abs() < 0.008 && rgba[off] < 0.002);
            assert!((rgba[2] - 0.07692).abs() < 0.007);
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
    camera.right = [0.8, 0., -0.6, 0.];
    let dark = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 8)?.with_nrd();
    let at = (32 * 65 + 32) * 16;
    let capture = Buffer::new(&mut gpu.context, at + 16, true)?;
    gpu.test_roots.push(capture.into());
    let mut radiance = [0.0_f32; 4];
    for _ in 0..12 {
        host.render_options(gpu, &output, &camera, dark, options)?;
        let pixel = surface_pixel(gpu, capture, at)?;
        for (sum, value) in radiance.iter_mut().zip(pixel) {
            *sum += value / 12.;
        }
    }
    let depth = gpu.view.as_ref().unwrap().rr.images[1].download(&mut gpu.context)?;
    let zi = (32 * 65 + 32) * 4;
    let z = f32::from_le_bytes(depth[zi..zi + 4].try_into()?);
    let transmittance = (-0.5 / (1.0_f32 - 0.4 * 0.4).sqrt()).exp();
    eprintln!("solid slab virtual_z={z} radiance={radiance:?} beer={transmittance}");
    assert!((z - 7.5).abs() < 0.002 && (0.89..0.95).contains(&radiance[0]));
    assert!((radiance[1] / radiance[0] - transmittance).abs() < 0.01 && radiance[2] < 0.01);
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
    let depth = view.rr.images[1].download(&mut gpu.context)?;
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
    let size = 32 * 32 * 112;
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
                (16 * 32 + 16) * 112 + lane * 56 + 32,
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
    let staging = Buffer::new(&mut gpu.context, samples * 112, true)?;
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
    view.signal_lanes
        .copy_prefix_to(&mut gpu.context, &staging, samples * 112)?;
    let mut lanes = vec![0u8; (samples * 112) as usize];
    staging.read(&mut gpu.context, 0, &mut lanes)?;
    for lane in lanes.chunks_exact_mut(56) {
        lane[32..38].fill(0);
    }
    staging.write(&mut gpu.context, 0, &lanes)?;
    staging.copy_prefix_to(&mut gpu.context, &view.signal_lanes, samples * 112)?;
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
