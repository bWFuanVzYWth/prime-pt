//! 直接消费宿主的源包并读取真正的 PT、NRD 与 RR 结果。
use super::*;
fn world(gpu: &mut Gpu) -> Result<(), Error> {
    gpu.replace_world_resources(
        ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [1, 1],
            albedo: &[255; 4],
            mers: &[0; 4],
            auxiliary: &[0; 4],
            materials: &[MaterialDefinition::default()],
            light_areas: &[[u32::MAX, 0, 0, 0]],
            emission_alias_words: &[],
            opacity_quads: &[],
        },
        b"PFM1\0\0\0\0",
        Default::default(),
    )
}
fn frame(gpu: &mut Gpu, faces: &[(u64, u32, [f32; 3], f32)]) -> Result<(), Error> {
    let mut src = Vec::new();
    src.extend_from_slice(b"PTF1");
    src.extend_from_slice(bytemuck::cast_slice(&[0u32, faces.len() as u32, 0]));
    src.extend_from_slice(&[0; 24]);
    for &(owner, flags, [x, y, z], radius) in faces {
        src.extend_from_slice(&owner.to_le_bytes());
        src.extend_from_slice(bytemuck::cast_slice(&[
            0u32,
            flags,
            u32::MAX,
            4,
            4,
            12,
            0,
            u32::MAX,
            u32::MAX,
        ]));
        for [u, v] in [[-1., -1.], [-1., 1.], [1., 1.], [1., -1.]] {
            src.extend_from_slice(bytemuck::cast_slice(&[x + u * radius, y + v * radius, z]));
        }
    }
    // SAFETY: 该合成帧只用白色纹理，不借外部视图。测试宿主在每帧后确认 GPU 完成。
    unsafe { gpu.set_world_features(&src) }
}
#[test]
#[ignore = "真实 Vulkan RT；逐纹素比较同一海晶灯的放置面与烘焙物品面"]
fn hardware_sea_lantern_item_emission() -> Result<(), Box<dyn std::error::Error>> {
    use super::super::features_tests::publish;
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let opts = Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd();
    let capture = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.test_roots.push(capture.into());
    let (albedo, spec) = if let Some(folder) = std::env::var_os("PRIME_SEA_FIXTURE") {
        let dir = std::path::PathBuf::from(folder);
        (
            std::fs::read(dir.join("sea_lantern.rgba8"))?,
            std::fs::read(dir.join("sea_lantern_s.rgba8"))?,
        )
    } else {
        (
            vec![255; 16 * 16 * 4],
            (0..256)
                .flat_map(|i| [0, 0, 0, if i < 128 { 255 } else { 160 }])
                .collect(),
        )
    };
    assert_eq!(albedo.len(), 1024);
    let mut atlas = vec![255; 32 * 16 * 4];
    for (dst, src) in atlas.chunks_exact_mut(128).zip(albedo.chunks_exact(64)) {
        dst[..64].copy_from_slice(src);
    }
    let mut catalog = prime_minecraft::Catalog::default();
    let mut staged = prime_minecraft::fixtures::sea_lantern(&mut catalog, Some(spec))?;
    let maps = staged.material_resources([32, 16], &atlas)?;
    let (mask, gain) = maps.item_materials.surface(Some(1), 1, 0, false).unwrap();
    assert!(mask != 0 && gain > 0.);
    let id = maps
        .materials
        .iter()
        .position(|m| {
            m.control[2] & MaterialDefinition::TEXTURED_EMISSION != 0 && m.emission[3] > 0.
        })
        .unwrap();
    let weights: Vec<_> = (0..256)
        .map(|i| maps.mers[((i / 16) * 32 + i % 16) * 4 + 1])
        .collect();
    let min = (0..256).min_by_key(|&i| weights[i]).unwrap();
    let max = (0..256).max_by_key(|&i| weights[i]).unwrap();
    assert!(weights[max] > weights[min]);
    gpu.replace_world_resources(
        ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: maps.extent,
            albedo: &atlas,
            mers: &maps.mers,
            auxiliary: &maps.auxiliary,
            materials: maps.materials,
            light_areas: &maps.quad_areas,
            emission_alias_words: &maps.emission_alias_words,
            opacity_quads: &maps.opacity_quads,
        },
        b"PFM1\0\0\0\0",
        maps.item_materials,
    )?;
    let vertices = [[-4f32, -4., 3.], [-4., 4., 3.], [4., 4., 3.], [4., -4., 3.]];
    let darkroom = |gpu: &mut Gpu| -> Result<[f32; 4], Box<dyn std::error::Error>> {
        gpu.invalidate()?;
        host.render_options(gpu, &output, &camera(), env, opts)?;
        Ok(linear_pixel(
            &mut gpu.context,
            gpu.view.as_ref().unwrap().prefix,
            capture,
            (16 * 32 + 16) * 16,
        )?)
    };
    for (case, i) in [min, max].into_iter().enumerate() {
        let uv = [
            (i % 16) as f32 / 32. + 0.5 / 32.,
            (i / 16) as f32 / 16. + 0.5 / 16.,
        ];
        frame(gpu, &[])?;
        let mesh = Mesh {
            quads: vec![Quad::with_material(
                vertices,
                [uv; 4],
                [1.; 3],
                prime_scene::MaterialId::new(id as u32),
            )],
        };
        publish(
            gpu,
            &mesh,
            2 * case as u64 + 1,
            2 * case as u64 + 1,
            [0.; 3],
        )?;
        let placed = darkroom(gpu)?;
        gpu.set_regions(prime_scene::RenderView {
            generation: gpu.resource_generation,
            anchor: [0.; 3],
            regions: &[],
        })?;
        let mut packet = b"PTF1".to_vec();
        packet.extend_from_slice(bytemuck::cast_slice(&[0u32, 1, 0]));
        packet.extend_from_slice(&[0; 24]);
        packet.extend_from_slice(&17u64.to_le_bytes());
        packet.extend_from_slice(bytemuck::cast_slice(&[
            0u32,
            0,
            u32::MAX,
            2,
            1,
            96,
            0,
            u32::MAX,
            u32::MAX,
            1,
        ]));
        packet.extend_from_slice(bytemuck::cast_slice(&[
            [1f32, 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
        ]));
        packet.extend_from_slice(bytemuck::cast_slice(&[u32::MAX - 1, 1, 0, u32::MAX]));
        for [x, y, z] in vertices {
            packet.extend_from_slice(bytemuck::cast_slice(&[x, y, z, uv[0], uv[1]]));
        }
        // SAFETY: 夹具只引用这个资源世代的方块图集；宿主逐帧等待设备。
        unsafe {
            gpu.set_world_features(&packet)?;
        }
        let item = darkroom(gpu)?;
        eprintln!(
            "海晶灯屏幕中心 发光遮罩 {}，放置 {:?}，物品 {:?}",
            weights[i],
            &placed[..3],
            &item[..3]
        );
        for k in 0..3 {
            assert!((item[k] - placed[k]).abs() <= 0.001 * placed[k].max(1.));
        }
        if i == max {
            assert!(item[0] > 0.1);
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan RT；动态与静态发光表面在 NEE 和路径命中中共享选择概率"]
fn hardware_dynamic_light_competition() -> Result<(), Box<dyn std::error::Error>> {
    use super::super::features_tests::{assert_light_pdfs, publish, read_light_probes, scene};
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let dark = MaterialDefinition::default();
    let lamp = MaterialDefinition {
        emission: [1., 1., 1., 5.],
        ..dark
    };
    gpu.replace_world_resources(
        ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [1, 1],
            albedo: &[255; 4],
            mers: &[0; 4],
            auxiliary: &[0; 4],
            materials: &[dark, lamp],
            light_areas: &[[u32::MAX, 0, 0, 0]; 2],
            emission_alias_words: &[],
            opacity_quads: &[],
        },
        b"PFM1\0\0\0\0",
        Default::default(),
    )?;
    let output = image(gpu)?;
    let mut mesh = scene();
    publish(gpu, &mesh, 1, 1, [0.; 3])?;
    let mut faces: Vec<_> = (0..97)
        .map(|i| {
            (
                i + 1,
                4,
                [2. + (i % 10) as f32 * 0.1, (i / 10) as f32 * 0.1, 2.],
                0.075,
            )
        })
        .collect();
    frame(gpu, &faces)?;
    let near = assert_light_pdfs(&host, gpu, &output, "相邻的两个灯域")?;
    assert!(near[0] > 0 && near[1] > 0, "{near:?}");
    for face in &mut faces {
        face.2[0] += 8.;
    }
    frame(gpu, &faces)?;
    let far = assert_light_pdfs(&host, gpu, &output, "动态灯移远")?;
    assert!(far[1] < near[1], "移近 {near:?}，移远 {far:?}");
    let mut isolated: Vec<_> = (0..64)
        .map(|i| (i + 1, 4, [60. + (i % 8) as f32, (i / 8) as f32, 3.], 0.4))
        .collect();
    isolated[0].2 = [2., 0., 3.];
    frame(gpu, &isolated)?;
    let samples = read_light_probes(&host, gpu, &output, 16_384)?;
    let n = samples.len() as f64;
    let mean = samples.iter().map(|x| f64::from(x[0])).sum::<f64>() / n;
    let variance = samples
        .iter()
        .map(|x| (f64::from(x[0]) - mean).powi(2))
        .sum::<f64>()
        / n;
    let close = samples.iter().filter(|x| x[1] == 1.).count();
    eprintln!(
        "局部性探针 样本 {n} 近灯频率 {:.5} 无遮挡直接光均值 {mean:.5} 单样本相对标准差 {:.5}",
        close as f64 / n,
        variance.sqrt() / mean
    );
    assert!(close as f64 / n > 0.25 && mean > 0. && variance.sqrt() / mean < 2.);
    frame(gpu, &[])?;
    assert_eq!(
        assert_light_pdfs(&host, gpu, &output, "移除动态灯")?,
        [32, 0]
    );
    mesh.quads.truncate(1);
    publish(gpu, &mesh, 2, 2, [0.; 3])?;
    frame(gpu, &faces[..1])?;
    assert_eq!(
        assert_light_pdfs(&host, gpu, &output, "只有一个动态灯")?,
        [0, 32]
    );
    let mut mesh = scene();
    let extra = mesh.quads[1];
    mesh.quads.push(Quad::with_material(
        extra.positions().map(|p| [p[0] + 2., p[1], p[2]]),
        extra.uv(),
        extra.tint(),
        extra.material_id(),
    ));
    // 静态区块单独换代，沿用上一帧的动态发光面与位置。
    publish(gpu, &mesh, 3, 3, [0.; 3])?;
    let counts = assert_light_pdfs(&host, gpu, &output, "热替换的多灯区块")?;
    assert!(counts[0] > 0);
    eprintln!(
        "32 个分层样本中的动态灯命中数：近处 {}，远处 {}",
        near[1], far[1]
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan RT；实时与离线消费按人称提取的场景，即时撤下前一帧的玩家"]
fn hardware_camera_mode_scene_replacement_matches_offline() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    world(gpu)?;
    let output = image(gpu)?;
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let opts = Options::new(false, streamline::Quality::Dlaa, 2, 4)?;
    for mode in [
        prime_scene::RenderMode::Realtime,
        prime_scene::RenderMode::Offline,
    ] {
        gpu.set_rendering(mode)?;
        let faces = [(1, 0, [0., 0., 0.1], 1.), (2, 4, [0., 0., 3.], 4.)];
        for third_person in [true, false, true] {
            frame(gpu, if third_person { &faces } else { &faces[1..] })?;
            gpu.invalidate()?;
            host.render_options(gpu, &output, &camera(), env, opts)?;
            let pixels = output.download(&mut gpu.context)?;
            let color = center(&pixels);
            assert!(
                color[..3]
                    .iter()
                    .all(|v| if third_person { *v < 5 } else { *v > 100 }),
                "{mode:?} 第三人称 {third_person}，中心 RGB {color:?}"
            );
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 PT 阴影掩码和相机命中"]
fn hardware_world_feature_shadow_visibility() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    world(gpu)?;
    let output = image(gpu)?;
    let eye = camera();
    let env = Environment {
        sun_direction: [0.28f32.sqrt(), 0.6, -0.6],
        ..Default::default()
    };
    let receiver = (1, 0, [0., 0., 3.], 5.);
    let seed = gpu.absolute_sample;
    let illuminate = |gpu: &mut Gpu, flags: u32| {
        frame(gpu, &[receiver, (2, flags, [0.95, 1., 2.], 0.5)])?;
        gpu.test_restart_sample_sequence(seed)?;
        host.render_environment(gpu, &output, &eye, env)
    };
    let opaque = center(&illuminate(gpu, 0)?).to_vec();
    let text = center(&illuminate(gpu, 1 | 8)?).to_vec();
    let beam = center(&illuminate(gpu, 2 | 4 | 8 | 32)?).to_vec();
    assert!(
        (0..3).all(|i| text[i] > opaque[i] + 8),
        "实体阴影 {opaque:?}，文字 {text:?}"
    );
    assert_eq!(text, beam);
    // 同一相机光路上的字形仍命中自己的深度。
    frame(gpu, &[receiver, (2, 1 | 8, [0., 0., 2.], 0.5)])?;
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd();
    host.render_options(gpu, &output, &eye, env, options)?;
    let z = gpu.view.as_ref().unwrap().rr.images[1].download(&mut gpu.context)?;
    let at = (16 * 32 + 16) * 4;
    let depth = f32::from_le_bytes(z[at..at + 4].try_into()?);
    assert!((depth - 2.).abs() < 0.002, "字形深度 {depth}");
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
fn feature_motion(rr: bool) -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(rr, true)?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    world(gpu)?;
    let extent = if std::env::var_os("PRIME_NRD_4K").is_some() {
        [3840, 2160]
    } else {
        [128, 96]
    };
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    let mut opts = Options::new(rr, streamline::Quality::Dlaa, 2, 4)?;
    if !rr {
        opts = opts.with_nrd();
    }
    let eye = camera();
    for delta in [0., 0., 0.125] {
        frame(gpu, &[(42, 0, [delta, 0., 4.], 2.)])?;
        host.render_options(gpu, &output, &eye, Environment::default(), opts)?;
    }
    let view = gpu.view.as_ref().unwrap();
    let mv = if rr {
        float_pixel(&view.rr.images[2].download(&mut gpu.context)?, extent, 2)
    } else {
        float_pixel(
            &view.rr.nrd_motion[0].download(&mut gpu.context)?,
            extent,
            4,
        )
    };
    let expected = -0.125 * extent[0] as f32 / 8.;
    eprintln!("动态平面 rr={rr} 分辨率={extent:?} motion={mv:?} expected={expected}");
    assert!((mv[0] - expected).abs() < 0.1 && mv[1].abs() < 0.1);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
#[test]
#[ignore = "NRD 动态表面的像素重投影"]
fn hardware_world_feature_nrd_motion() -> Result<(), Box<dyn std::error::Error>> {
    feature_motion(false)
}
#[test]
#[ignore = "DLAA 动态表面的像素重投影"]
fn hardware_world_feature_rr_motion() -> Result<(), Box<dyn std::error::Error>> {
    feature_motion(true)
}

#[test]
#[ignore = "实际纹理采样、刚性组件 BLAS、旋转重投影和反射"]
fn hardware_world_bone_texture_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, true)?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = if std::env::var_os("PRIME_NRD_4K").is_some() {
        [3840, 2160]
    } else {
        [160, 96]
    };
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    let tex = Image::new(&mut gpu.context, [2, 1, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots
        .extend([output, tex].map(vulkan::native::Resource::from));
    tex.upload(&mut gpu.context, &[255, 0, 0, 0, 0, 255, 0, 255])?;
    let mut catalog = b"PFM1".to_vec();
    catalog.extend_from_slice(bytemuck::cast_slice(&[1u32, 1]));
    for v in [
        [-3f32, -3., 0., 0., 0.],
        [-3., 3., 0., 0., 1.],
        [3., 3., 0., 1., 1.],
        [3., -3., 0., 1., 0.],
    ] {
        catalog.extend_from_slice(bytemuck::cast_slice(&v));
    }
    let mut mirror = MaterialDefinition::default();
    mirror.base_roughness = [1., 1., 1., 0.];
    mirror.surface[1] = 1.;
    mirror.control[2] = 0;
    gpu.replace_world_resources(
        ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [1, 1],
            albedo: &[255; 4],
            mers: &[0; 4],
            auxiliary: &[0; 4],
            materials: &[mirror],
            light_areas: &[[u32::MAX, 0, 0, 0]],
            emission_alias_words: &[],
            opacity_quads: &[],
        },
        &catalog,
        Default::default(),
    )?;
    let view = tex.raw(&gpu.context)?.view;
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?.with_nrd();
    let eye = camera();
    let render = |gpu: &mut Gpu,
                  angle: f32,
                  shift: f32,
                  z: f32,
                  revision: u64|
     -> Result<(), Box<dyn std::error::Error>> {
        let (s, c) = angle.sin_cos();
        let mut packet = b"PTF1".to_vec();
        packet.extend_from_slice(bytemuck::cast_slice(&[1u32, 1, 0]));
        packet.extend_from_slice(&[0; 24]);
        packet.extend_from_slice(bytemuck::cast_slice(&[view, 123u64, revision]));
        packet.extend_from_slice(bytemuck::cast_slice(&[2u32, 1, 0, 0]));
        packet.extend_from_slice(&32u64.to_le_bytes());
        packet.extend_from_slice(bytemuck::cast_slice(&[
            0u32,
            1 | 4 | 8,
            0,
            0,
            1,
            64,
            0,
            u32::MAX,
            u32::MAX,
        ]));
        packet.extend_from_slice(&47u64.to_le_bytes());
        packet.extend_from_slice(bytemuck::cast_slice(&[0u32, u32::MAX]));
        packet.extend_from_slice(bytemuck::cast_slice(&[
            [c, 0., s, shift],
            [0., 1., 0., 0.],
            [-s, 0., c, z],
        ]));
        // SAFETY: tex 由真实测试宿主保活于同设备，前次提交已完成。
        unsafe {
            gpu.set_world_features(&packet)?;
        }
        host.render_options(gpu, &output, &eye, Environment::default(), options)?;
        Ok(())
    };
    render(gpu, 0., 0., 4., 1)?;
    render(gpu, 0., 0., 4., 1)?;
    let angle = 0.22f32;
    let shift = 0.125;
    render(gpu, angle, shift, 4., 1)?;
    let guide = gpu.view.as_ref().unwrap();
    let depth = guide.rr.images[1].download(&mut gpu.context)?;
    let motion = guide.rr.nrd_motion[0].download(&mut gpu.context)?;
    let [w, h] = extent;
    let y = h / 2;
    let at = |x: u32| (y * w + x) as usize;
    let z_at = |x| float_component(&depth, at(x), 4);
    eprintln!(
        "模型 alpha mask depth left={} right={} center={}",
        z_at(3 * w / 8),
        z_at(5 * w / 8),
        z_at(w / 2)
    );
    assert!(z_at(3 * w / 8) > 1000. && (z_at(5 * w / 8) - 4.).abs() < 0.4);
    let mut worst = 0f32;
    for x in [5 * w / 8, 9 * w / 16, 19 * w / 32] {
        let slope = 2. * (x as f32 + 0.5) / w as f32 - 1.;
        let (s, c) = angle.sin_cos();
        let old_x = (4. * slope - shift) / (c + slope * s);
        let expected = (old_x / 4. - slope) * 0.5 * w as f32;
        worst = worst.max((float_component(&motion, at(x) * 4, 4) - expected).abs());
    }
    eprintln!("刚性部件转动 {extent:?} 三个采样点最大误差 {worst:.4} px");
    assert!(worst < 0.2);
    // 仅改变宿主纹理内容及版本，命中掩码随之换边。
    tex.upload(&mut gpu.context, &[0, 255, 0, 255, 255, 0, 0, 0])?;
    render(gpu, 0., 0., 4., 2)?;
    let depth = gpu.view.as_ref().unwrap().rr.images[1].download(&mut gpu.context)?;
    assert!(
        (float_component(&depth, at(3 * w / 8), 4) - 4.).abs() < 0.01
            && float_component(&depth, at(5 * w / 8), 4) > 1000.
    );
    // 模型放在相机背后，镜面应反射其绿色发光纹素。
    let mesh = Mesh {
        quads: vec![plane(-8., 8., 2., 0)],
    };
    gpu.set_regions(prime_scene::RenderView {
        generation: gpu.resource_generation,
        regions: &[prime_scene::RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }],
        anchor: [0.; 3],
    })?;
    render(gpu, 0., 2., -2., 2)?;
    let prefix = gpu.view.as_ref().unwrap().prefix;
    let pickup = Buffer::new(&mut gpu.context, 16 * u64::from(w) * u64::from(h), true)?;
    let color = linear_pixel(&mut gpu.context, prefix, pickup, (at(w / 2) * 16) as u64)?;
    assert!(
        color[1] > 0.3 && color[0] < 0.01 && color[2] < 0.01,
        "镜面发光 {color:?}"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "有限距离的实际局部发光面与直射阴影"]
fn hardware_world_feature_local_shadow() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    gpu.replace_world_resources(
        ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [1, 1],
            albedo: &[255; 4],
            mers: &[0; 4],
            auxiliary: &[0; 4],
            materials: &[emitter([16., 0., 0.])],
            light_areas: &[[u32::MAX, 0, 0, 0]],
            emission_alias_words: &[],
            opacity_quads: &[],
        },
        b"PFM1\0\0\0\0",
        Default::default(),
    )?;
    let lamp = Quad::with_material(
        [
            [2., -0.4, 1.],
            [2., 0.4, 1.],
            [2.8, 0.4, 1.],
            [2.8, -0.4, 1.],
        ],
        [[0.5; 2]; 4],
        [1.; 3],
        prime_scene::MaterialId::new(0),
    );
    let mesh = Mesh { quads: vec![lamp] };
    gpu.set_regions(prime_scene::RenderView {
        generation: gpu.resource_generation,
        regions: &[prime_scene::RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }],
        anchor: [0.; 3],
    })?;
    let env = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let seed = gpu.absolute_sample;
    let receiver = (1, 0, [0., 0., 4.], 4.);
    let direct = Options::new(false, streamline::Quality::Dlaa, 8, 1)?;
    let capture = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.test_roots.push(capture.into());
    let light =
        |gpu: &mut Gpu, blocker: Option<u32>| -> Result<[f32; 4], Box<dyn std::error::Error>> {
            let mut faces = vec![receiver];
            if let Some(flags) = blocker {
                faces.push((2, flags, [1.2, 0., 2.5], 0.8));
            }
            frame(gpu, &faces)?;
            gpu.test_restart_sample_sequence(seed)?;
            host.render_options(gpu, &output, &camera(), env, direct)?;
            Ok(surface_pixel(gpu, capture, (16 * 32 + 16) * 16)?)
        };
    let clear = light(gpu, None)?;
    let opaque = light(gpu, Some(0))?;
    let glyph = light(gpu, Some(1 | 8))?;
    let beam = light(gpu, Some(2 | 4 | 8 | 32))?;
    eprintln!("局部光接收面 clear={clear:?} opaque={opaque:?} text={glyph:?} beam={beam:?}");
    // 灯发红光，光柱自身是白光；用色差单独测接收面上的局部灯贡献。
    let red = |c: [f32; 4]| c[0] - c[1];
    assert!(red(clear) > 0.01 && red(opaque) < 0.1 * red(clear));
    for visible in [glyph, beam] {
        assert!((red(visible) / red(clear) - 1.).abs() < 0.04);
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "原版粒子姿态、多个面及出生时的时域历史"]
fn hardware_world_particle_motion() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, true)?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    world(gpu)?;
    let [w, h] = if std::env::var_os("PRIME_NRD_4K").is_some() {
        [3840, 2160]
    } else {
        [160, 96]
    };
    let output = Image::new(&mut gpu.context, [w, h, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let opts = Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd();
    let render =
        |gpu: &mut Gpu, id: u64, roll: f32, shift: f32| -> Result<(), Box<dyn std::error::Error>> {
            let mut packet = b"PTF1".to_vec();
            packet.extend_from_slice(bytemuck::cast_slice(&[0u32, 1, 0]));
            packet.extend_from_slice(&[0; 24]);
            packet.extend_from_slice(&0u64.to_le_bytes());
            packet.extend_from_slice(bytemuck::cast_slice(&[
                0u32,
                1,
                u32::MAX,
                1,
                2,
                64,
                0,
                u32::MAX,
                u32::MAX,
            ]));
            let (s, c) = (roll * 0.5).sin_cos();
            for part in 0..2u32 {
                packet.extend_from_slice(&id.to_le_bytes());
                packet.extend_from_slice(bytemuck::cast_slice(&[
                    shift + 6. * part as f32,
                    0.,
                    4.,
                    0.,
                    0.,
                    s,
                    c,
                    1.,
                    0.,
                    1.,
                    0.,
                    1.,
                ]));
                packet.extend_from_slice(bytemuck::cast_slice(&[u32::MAX, part << 8]));
            }
            // SAFETY: 两个面均使用白色，测试宿主已完成上帧。
            unsafe {
                gpu.set_world_features(&packet)?;
            }
            host.render_options(gpu, &output, &camera(), Environment::default(), opts)?;
            Ok(())
        };
    render(gpu, 37, 0., 0.)?;
    render(gpu, 37, 0., 0.)?;
    let roll = 0.32f32;
    let shift = 0.125;
    render(gpu, 37, roll, shift)?;
    let (x, y) = (w * 9 / 16, h / 2);
    let index = (y * w + x) as usize * 4;
    let uv = [
        (x as f32 + 0.5) * 2. / w as f32 - 1.,
        1. - (y as f32 + 0.5) * 2. / h as f32,
    ];
    let (s, c) = roll.sin_cos();
    let dx = 4. * uv[0] - shift;
    let dy = 4. * uv[1];
    let expected = [
        (0.25 * (c * dx + s * dy) - uv[0]) * 0.5 * w as f32,
        (uv[1] - 0.25 * (-s * dx + c * dy)) * 0.5 * h as f32,
    ];
    let motion = |gpu: &mut Gpu| -> Result<[f32; 2], Error> {
        let data = gpu.view.as_ref().unwrap().rr.nrd_motion[0].download(&mut gpu.context)?;
        Ok([
            float_component(&data, index, 4),
            float_component(&data, index + 1, 4),
        ])
    };
    let actual = motion(gpu)?;
    let error = (actual[0] - expected[0]).hypot(actual[1] - expected[1]);
    assert!(
        error < 0.2,
        "粒子转动的 motion {actual:?}，期望 {expected:?}"
    );
    render(gpu, 38, roll, shift)?;
    let born = motion(gpu)?;
    assert!(born[0].abs() >= w as f32 && born[1].abs() >= h as f32);
    eprintln!("粒子转动原生 {w}x{h} 误差 {error:.4} px，新粒子拒绝旧历史 {born:?}");
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
