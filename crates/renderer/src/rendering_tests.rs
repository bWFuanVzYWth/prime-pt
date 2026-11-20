//! 新积分器通过真实宿主命令录制、完成和读回；不启动 Minecraft。
use super::*;
use prime_scene::{RenderRegion, RenderView};

fn publish(gpu: &mut Gpu, mesh: &Mesh, content: u64) -> Result<(), Error> {
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: content,
            mesh: mesh.into(),
        }],
    })
}

#[test]
#[ignore = "独立进程：原生4K DLAA+SER，移动相机和连续区块增删后仍能完成有限HDR"]
fn hardware_pt_rr_streaming_motion() -> Result<(), Box<dyn std::error::Error>> {
    use prime_scene::{MaterialDefinition, MaterialId, RenderMode};
    let host = Host::new_rr_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = Image::new(&mut gpu.context, [3840, 2160, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut definitions = [MaterialDefinition::default(); 12];
    for material in &mut definitions {
        material.control = [0, 3, 0, 0];
    }
    definitions[1].surface[1] = 1.;
    definitions[1].base_roughness[3] = 0.15;
    definitions[2].control[0] = 1;
    definitions[2].base_roughness[3] = 0.;
    definitions[3].control[0] = 1;
    definitions[3].extinction = [1.33, 0.03, 0.01, 0.005];
    definitions[3].base_roughness[3] = 0.15;
    definitions[4].surface[2] = 0.5;
    definitions[5].emission = [2., 1., 0.5, 1.];
    definitions[6].control[2] = MaterialDefinition::CUTOUT;
    definitions[7].control[2] = MaterialDefinition::AUXILIARY;
    definitions[7].control[3] = MaterialDefinition::AUTHORED_NORMAL;
    definitions[8] = definitions[7];
    definitions[8].surface[2] = 0.6;
    definitions[9] = definitions[3];
    definitions[9].control[2] = MaterialDefinition::TRANSLUCENT;
    definitions[10].surface[1] = 0.4;
    definitions[10].base_roughness[3] = 0.2;
    definitions[11].control = [3, 3, MaterialDefinition::TRANSLUCENT, 0];
    definitions[11].base_roughness[3] = 0.;
    let mut albedo = Vec::new();
    let mut auxiliary = Vec::new();
    for y in 0..8 {
        for x in 0..8 {
            albedo.extend([200, 180, 150, if (x + y) % 2 == 0 { 255 } else { 0 }]);
            auxiliary.extend([100 + x * 8, 100 + y * 8, 0, 85]);
        }
    }
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [8, 8],
        albedo: &albedo,
        materials: &definitions,
        mers: &[0; 256],
        auxiliary: &auxiliary,
        emission_alias_words: &[],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 12],
    })?;
    gpu.set_rendering(RenderMode::Realtime)?;
    gpu.set_render_tuning(prime_scene::RenderTuning::new(false))?;
    let mut meshes = Vec::new();
    for variant in 0..3 {
        let mut mesh = Mesh::default();
        for y in 0..16 {
            for x in 0..32 {
                if variant == 1 && (x + y) % 7 == 0 {
                    continue;
                }
                let z = 20. + (x % 3) as f32 + variant as f32;
                let a = x as f32 * 2.;
                let b = y as f32 * 2.;
                mesh.quads.push(Quad::with_material(
                    [
                        [a, b, z],
                        [a, b + 2., z],
                        [a + 2., b + 2., z],
                        [a + 2., b, z],
                    ],
                    [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
                    [0.8, 0.7, 0.6],
                    MaterialId::new(((x + y + variant) % 12) as u32),
                ));
            }
        }
        for points in [
            [[0., 0., 0.], [0., 0., 64.], [64., 0., 64.], [64., 0., 0.]],
            [
                [0., 0., 40.],
                [0., 32., 40.],
                [64., 32., 40.],
                [64., 0., 40.],
            ],
        ] {
            mesh.quads.push(Quad::with_material(
                points,
                [[0.5; 2]; 4],
                [0.8; 3],
                MaterialId::new(0),
            ));
        }
        meshes.push(mesh);
    }
    let quality = match std::env::var("PRIME_PT_STREAMING_QUALITY").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("dlaa") => streamline::Quality::Dlaa,
        Ok("performance") => streamline::Quality::Performance,
        _ => return Err("PRIME_PT_STREAMING_QUALITY 只接受 dlaa 或 performance".into()),
    };
    let options = Options::new(true, quality, 1, 12)?;
    let frames =
        std::env::var("PRIME_PT_STREAMING_FRAMES").map_or(Ok(240), |value| value.parse::<u64>())?;
    if frames == 0 {
        return Err("移动回归至少需要一帧".into());
    }
    eprintln!("移动回归：output=3840x2160 quality={quality:?} frames={frames}");
    for frame in 0..frames {
        let t = frame as f32 * 0.07;
        let x = 128. + 150. * t.sin();
        let center = (x / 64.).floor() as i32;
        let regions: Vec<_> = (center - 2..=center + 2)
            .map(|id| {
                let revision = frame / 9 + 1;
                RenderRegion {
                    key: [id, 0, 0],
                    geometry_revision: revision,
                    content_revision: revision,
                    mesh: (&meshes[revision as usize % 3]).into(),
                }
            })
            .collect();
        gpu.set_regions(RenderView {
            generation: gpu.resource_generation,
            anchor: [f64::from(center) * 64., 0., 0.],
            regions: &regions,
        })?;
        let mut camera = camera();
        camera.origin = [
            x - center as f32 * 64.,
            8. + 3. * (t * 0.7).sin(),
            5. + 3. * t.cos(),
            0.,
        ];
        camera.right[0] = 16. / 9.;
        if frame % 16 == 0 {
            eprintln!("streaming frame={frame} center={center} begin");
        }
        host.render_options(&mut gpu, &output, &camera, Environment::default(), options)?;
        assert!(gpu.rr_unavailable.is_none() && gpu.rr_previous.is_some());
        if frame % 16 == 0 {
            eprintln!(
                "streaming frame={frame} complete hdr={:?}",
                hdr_mean(&mut gpu)?
            );
        }
    }
    let hdr = gpu.view.as_ref().unwrap().output;
    eprintln!(
        "streaming final_hdr_blake3={}",
        blake3::hash(&download_buffer(&mut gpu, hdr)?),
    );
    gpu.close()?;
    session.finish()?;
    // RR 的 SDK 验证消息单独保留；此用例检查真实求值、提交完成及 HDR，不将已知内部消息作为设备丢失。
    eprintln!(
        "streaming validation_errors={}",
        host.errors.load(Ordering::Relaxed)
    );
    Ok(())
}

fn hdr_mean(gpu: &mut Gpu) -> Result<[f64; 3], Error> {
    let output = gpu.view.as_ref().unwrap().output;
    let readback = Buffer::new(&mut gpu.context, output.len(), true)?;
    output.copy_prefix_to(&mut gpu.context, &readback, output.len())?;
    let mut bytes = vec![0; output.len() as usize];
    readback.read(&mut gpu.context, 0, &mut bytes)?;
    let mut sum = [0.; 3];
    for pixel in bytes.as_chunks::<16>().0 {
        for channel in 0..3 {
            let value = f32::from_le_bytes(pixel[channel * 4..channel * 4 + 4].try_into().unwrap());
            assert!(value.is_finite());
            sum[channel] += f64::from(value);
        }
    }
    Ok(sum.map(|v| v / (bytes.len() / 16) as f64))
}

fn download_buffer(gpu: &mut Gpu, source: Buffer) -> Result<Vec<u8>, Error> {
    let readback = Buffer::new(&mut gpu.context, source.len(), true)?;
    source.copy_prefix_to(&mut gpu.context, &readback, source.len())?;
    let mut bytes = vec![0; source.len() as usize];
    readback.read(&mut gpu.context, 0, &mut bytes)?;
    Ok(bytes)
}

#[test]
#[ignore = "真实Vulkan RT；未编码和取消记录不能发布绝对随机身份"]
fn hardware_pt_cancelled_recording_keeps_absolute_sample_sequence()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    gpu.settings.samples = 2;
    let records = gpu.record(
        [32, 32],
        &camera(),
        Environment::default(),
        Binding::StorageImage(output),
    )?;
    assert_eq!(gpu.absolute_sample, 0);
    assert!(matches!(
        gpu.complete_previous(),
        Err(Error::Settings(pt::Error::PendingBatch))
    ));
    assert!(
        gpu.record(
            [32, 32],
            &camera(),
            Environment::default(),
            Binding::StorageImage(output)
        )
        .is_err()
    );
    gpu.cancel_recordings(records)?;
    assert_eq!(gpu.absolute_sample, 0);
    host.render(&mut gpu, &output, &camera())?;
    assert_eq!(gpu.absolute_sample, 2);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；原生诊断拒绝RR和跨帧RGB写入，保持逐帧独立采样"]
fn hardware_pt_diagnostic_bypasses_reconstruction_and_accumulation()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8, 0.6, 0.4], 4.);
    publish(&mut gpu, &mesh, 1)?;
    let options = Options::new(false, streamline::Quality::Performance, 8, 12)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options,
    )?;
    let accumulation = gpu.view.as_ref().unwrap().accumulation;
    let counts = gpu.view.as_ref().unwrap().counts;
    gpu.set_render_tuning(prime_scene::RenderTuning::new(true))?;
    let sequence_start = gpu.absolute_sample;
    let mut displays = Vec::new();
    for frame in 0..3 {
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            Options::new(true, streamline::Quality::Performance, 4, 12)?,
        )?;
        let view = gpu.view.as_ref().unwrap();
        assert_eq!(view.extent, [32, 32]);
        assert!(!view.rr_enabled && gpu.rr_runtime.is_none());
        assert_eq!(gpu.accumulated_samples(), 2);
        assert_eq!(view.accumulation, accumulation);
        assert_eq!(view.counts, counts);
        assert_eq!(gpu.absolute_sample, sequence_start + (frame + 1) * 2);
        assert!(
            download_buffer(&mut gpu, counts)?
                .as_chunks::<4>()
                .0
                .iter()
                .all(|v| u32::from_le_bytes(*v) == 2)
        );
        displays.push(output.download(&mut gpu.context)?);
    }
    assert!(
        displays.windows(2).any(|pair| pair[0] != pair[1]),
        "每帧使用新的随机样本"
    );
    gpu.set_render_tuning(prime_scene::RenderTuning::DEFAULT)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options,
    )?;
    assert_eq!(gpu.accumulated_samples(), 2);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；2 SPP 诊断逐像素等于同一身份的两个离线单样本的均值"]
fn hardware_pt_diagnostic_batch_matches_single_sample_mean()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut expected = Vec::<Vec<f64>>::new();
    for spp in [1, 2] {
        let mut session = host.gpu()?;
        let mut gpu = session.gpu();
        let output = image(&mut gpu)?;
        let mut mesh = Mesh::default();
        add_quad(&mut mesh, 2., [0.5; 2], [0.8, 0.6, 0.4], 4.);
        publish(&mut gpu, &mesh, 1)?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(true))?;
        if spp == 1 {
            gpu.set_rendering(prime_scene::RenderMode::Offline)?;
        }
        for frame in 0..8 / spp {
            if spp == 1 {
                gpu.invalidate()?;
            }
            host.render_options(
                &mut gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, spp, 12)?,
            )?;
            let hdr = gpu.view.as_ref().unwrap().output;
            let bytes = download_buffer(&mut gpu, hdr)?;
            let values: Vec<_> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| f64::from(f32::from_le_bytes(*v)))
                .collect();
            if spp == 1 {
                if frame % 2 == 0 {
                    expected.push(vec![0.; values.len()]);
                }
                for (sum, value) in expected.last_mut().unwrap().iter_mut().zip(values) {
                    *sum += value / 2.;
                }
            } else {
                for (actual, target) in values.iter().zip(&expected[frame as usize]) {
                    assert!(
                        (actual - target).abs() <= 5e-6 * target.abs().max(1.),
                        "2 SPP={actual}, 1 SPP均值={target}"
                    );
                }
            }
        }
        assert_eq!(gpu.absolute_sample, 8);
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；离线继承曝光且只在调节强度变化时重测"]
fn hardware_pt_offline_preserves_exposure() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    gpu.set_display_options(0.65, 0.05, 0.75, 0.4)?;
    let live = Options::new(false, streamline::Quality::Dlaa, 1, 6)?;
    host.render_options(&mut gpu, &output, &camera(), Environment::default(), live)?;
    let inherited = exposure_state(&mut gpu)?;
    gpu.set_rendering(prime_scene::RenderMode::Offline)?;
    let batch = Options::new(false, streamline::Quality::Dlaa, 8, 6)?;
    for _ in 0..3 {
        host.render_options(&mut gpu, &output, &camera(), Environment::default(), batch)?;
    }
    assert_eq!(
        inherited,
        exposure_state(&mut gpu)?,
        "冻结画面不继续时域测光"
    );
    gpu.set_display_options(0.3, 0.05, 0.75, 0.4)?;
    host.render_options(&mut gpu, &output, &camera(), Environment::default(), batch)?;
    let changed = exposure_state(&mut gpu)?;
    assert_ne!(inherited, changed);
    host.render_options(&mut gpu, &output, &camera(), Environment::default(), batch)?;
    assert_eq!(changed, exposure_state(&mut gpu)?);
    assert_eq!(gpu.accumulated_samples(), 40, "显示设置不清空离线线性累积");
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独立进程：真实4K/Performance RR+SER+CV穿过delta首面与不同Guide材质，严格保留SDK同步检查"]
fn hardware_pt_rr_ser_guided_suffix_across_materials() -> Result<(), Box<dyn std::error::Error>> {
    guided_suffix_across_materials(streamline::Quality::Performance, true)
}

#[test]
#[ignore = "完整4K原生Guide，PT与SER热切换；不执行SDK evaluate，独立检查引擎同步"]
fn hardware_pt_4k_guided_suffix_without_sdk_evaluation() -> Result<(), Box<dyn std::error::Error>> {
    guided_suffix_across_materials(streamline::Quality::Dlaa, false)
}

fn guided_suffix_across_materials(
    quality: streamline::Quality,
    evaluate: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use prime_scene::{MaterialDefinition, MaterialId};
    let host = Host::new_rr_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    assert!(gpu.invocation_reorder);
    let output = Image::new(&mut gpu.context, [3840, 2160, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut opaque = MaterialDefinition::default();
    opaque.control[2] |= MaterialDefinition::AUXILIARY;
    opaque.control[3] = MaterialDefinition::AUTHORED_NORMAL;
    let mut glass = MaterialDefinition::default();
    glass.control[0] = 1;
    glass.base_roughness[3] = 0.;
    let mut lamp = opaque;
    lamp.emission = [2., 1., 0.5, 2.];
    let definitions = [opaque, glass, lamp];
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [1, 1],
        albedo: &[255; 4],
        materials: &definitions,
        mers: &[0, 0, 255, 0],
        auxiliary: &[128, 128, 0, 85],
        emission_alias_words: &[],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 3],
    })?;
    gpu.set_rendering(prime_scene::RenderMode::Realtime)?;
    gpu.set_render_tuning(prime_scene::RenderTuning::new(false))?;
    let mut mesh = Mesh::default();
    let uv = [[0., 0.], [0., 1.], [1., 1.], [1., 0.]];
    for (z, material) in [(1., 1), (3., 0)] {
        mesh.quads.push(Quad::with_material(
            [
                [-10., -10., z],
                [-10., 10., z],
                [10., 10., z],
                [10., -10., z],
            ],
            uv,
            [0.8; 3],
            MaterialId::new(material),
        ));
    }
    mesh.quads.push(Quad::with_material(
        [
            [-10., -10., -2.],
            [10., -10., -2.],
            [10., 10., -2.],
            [-10., 10., -2.],
        ],
        uv,
        [0.8; 3],
        MaterialId::new(0),
    ));
    mesh.quads.push(Quad::with_material(
        [[-2., 2., -2.], [2., 2., -2.], [2., 2., 2.], [-2., 2., 2.]],
        uv,
        [1.; 3],
        MaterialId::new(2),
    ));
    publish(&mut gpu, &mesh, 1)?;
    let mut camera = camera();
    {
        gpu.set_rendering(prime_scene::RenderMode::Realtime)?;
        publish(&mut gpu, &mesh, 2)?;
        for rr in [false, true] {
            for (frame, ser) in [false, true, false, true].into_iter().enumerate() {
                let accepted = (gpu.absolute_sample, gpu.rr_index, gpu.rr_previous.is_some());

                gpu.set_ser(ser)?;
                assert_eq!(
                    accepted,
                    (gpu.absolute_sample, gpu.rr_index, gpu.rr_previous.is_some())
                );
                {}
                camera.origin[0] = frame as f32 * 0.05;
                eprintln!("RR+SER mode_rr={rr} frame={frame} begin ser={ser}");
                if evaluate {
                    host.render_options(
                        &mut gpu,
                        &output,
                        &camera,
                        Environment::default(),
                        Options::new(rr, quality, 1, 12)?,
                    )?;
                } else {
                    gpu.settings.samples = 1;
                    gpu.settings.max_bounces = 12;
                    gpu.rr_quality = quality;
                    let records = gpu.record_frame(
                        [3840, 2160],
                        &camera,
                        Environment::default(),
                        Binding::StorageImage(output),
                        rr,
                    )?;
                    for record in records {
                        record.submit_and_wait(&mut gpu.context)?;
                        gpu.recording_encoded();
                    }
                    gpu.complete_previous()?;
                }
                let view = gpu.view.as_ref().unwrap();
                assert_eq!(view.rr_enabled, rr);
                if !rr {
                    assert_eq!(view.extent, [3840, 2160]);
                    assert!(gpu.rr_runtime.is_none());
                    eprintln!(
                        "RR+SER mode_rr={rr} frame={frame} complete ser={ser} absolute={} rr_index={}",
                        gpu.absolute_sample, gpu.rr_index
                    );
                    continue;
                }
                if evaluate {
                    assert!(
                        gpu.rr_unavailable.is_none() && gpu.rr_previous.is_some(),
                        "必须实际完成SDK求值，不能只走原生回退"
                    );
                }
                assert_eq!(
                    view.extent,
                    if quality == streamline::Quality::Performance {
                        [1920, 1080]
                    } else {
                        [3840, 2160]
                    }
                );
                let extent = view.extent;
                let offset = (extent[1] / 2 * extent[0] + extent[0] / 2) as usize * 4;
                let depth = view.rr.images[1].download(&mut gpu.context)?;
                let depth = f32::from_le_bytes(depth[offset..offset + 4].try_into()?);
                let normal = half_pixel(&view.rr.images[3].download(&mut gpu.context)?, extent, 4);
                let diffuse = half_pixel(&view.rr.images[4].download(&mut gpu.context)?, extent, 4);
                let specular =
                    half_pixel(&view.rr.images[5].download(&mut gpu.context)?, extent, 4);
                let motion = half_pixel(&view.rr.images[2].download(&mut gpu.context)?, extent, 2);
                let specular_motion =
                    half_pixel(&view.rr.images[8].download(&mut gpu.context)?, extent, 2);
                let reflected = view.rr.images[7].download(&mut gpu.context)?;
                let reflected = f32::from_le_bytes(reflected[offset..offset + 4].try_into()?);
                assert!(
                    depth.is_finite() && depth > 1.5 && depth < 6.,
                    "透射Guide必须越过z=1的glass首面并恢复opaque终点：{depth}"
                );
                assert!(
                    (normal[3] - 0.9).abs() < 0.001,
                    "Guide终点应读取opaque粗糙度，不能停在delta首面：{normal:?}"
                );
                assert!(diffuse[..3].iter().all(|&v| v.is_finite() && v > 0.));
                assert!(
                    normal
                        .iter()
                        .chain(&specular)
                        .chain(&specular_motion)
                        .all(|v| v.is_finite())
                );
                assert!(
                    (reflected - 3.).abs() < 0.01,
                    "反射Guide必须命中相机后z=-2的不同材质：{reflected}"
                );
                assert!(motion.iter().all(|v| v.is_finite()));
                eprintln!(
                    "RR+SER mode_rr={rr} frame={frame} complete ser={ser} depth={depth} roughness={} reflection={reflected} motion={motion:?} specular={specular:?} specular_motion={specular_motion:?}",
                    normal[3]
                );
            }
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
