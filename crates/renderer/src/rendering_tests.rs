//! 新积分器通过真实宿主命令录制、完成和读回；不启动 Minecraft。
use super::*;
use prime_scene::{RealtimeRenderer, RenderRegion, RenderView};

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
#[ignore = "真实Vulkan RT；原生诊断拒绝RR和跨帧RGB写入，保持逐帧独立采样"]
fn hardware_pt_diagnostic_bypasses_reconstruction_and_accumulation()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    gpu.set_render_tuning(prime_scene::RenderTuning::new(20, 0., true, false)?)?;
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
        assert_eq!(gpu.accumulated_samples(), 4);
        assert_eq!(view.accumulation, accumulation);
        assert_eq!(view.counts, counts);
        assert_eq!(gpu.diagnostic_sample, (frame + 1) * 4);
        assert!(
            download_buffer(&mut gpu, counts)?
                .as_chunks::<4>()
                .0
                .iter()
                .all(|v| u32::from_le_bytes(*v) == 4)
        );
        displays.push(output.download(&mut gpu.context)?);
    }
    assert!(
        displays.windows(2).any(|pair| pair[0] != pair[1]),
        "每帧使用新的随机样本"
    );
    gpu.set_render_tuning(prime_scene::RenderTuning::BASELINE)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options,
    )?;
    assert_eq!(gpu.accumulated_samples(), 8);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；4 SPP诊断逐像素等于同一身份的四个1 SPP帧均值"]
fn hardware_pt_diagnostic_batch_matches_single_sample_mean()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut expected = Vec::<Vec<f64>>::new();
    for spp in [1, 4] {
        let mut gpu = host.gpu()?;
        let output = image(&mut gpu)?;
        let mut mesh = Mesh::default();
        add_quad(&mut mesh, 2., [0.5; 2], [0.8, 0.6, 0.4], 4.);
        publish(&mut gpu, &mesh, 1)?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(20, 0., true, false)?)?;
        for frame in 0..8 / spp {
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
                if frame % 4 == 0 {
                    expected.push(vec![0.; values.len()]);
                }
                for (sum, value) in expected.last_mut().unwrap().iter_mut().zip(values) {
                    *sum += value / 4.;
                }
            } else {
                for (actual, target) in values.iter().zip(&expected[frame as usize]) {
                    assert!(
                        (actual - target).abs() <= 5e-6 * target.abs().max(1.),
                        "4 SPP={actual}, 1 SPP均值={target}"
                    );
                }
            }
        }
        assert_eq!(gpu.diagnostic_sample, 8);
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；fixed fresh输出选择不反馈reservoir历史"]
fn hardware_pt_restir_fresh_selection_preserves_path_history()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    let mut histories = Vec::new();
    let mut outputs = Vec::new();
    for probability in [0., 0.4, 1.] {
        gpu.clear_world()?;
        gpu.set_rendering(
            prime_scene::RenderMode::Realtime,
            RealtimeRenderer::RestirPt,
        )?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(
            20,
            probability,
            false,
            false,
        )?)?;
        publish(&mut gpu, &mesh, 1)?;
        for _ in 0..8 {
            host.render_options(
                &mut gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, 1, 6)?,
            )?;
        }
        let history = gpu.restir.as_ref().unwrap().history_buffer();
        histories.push(download_buffer(&mut gpu, history)?);
        outputs.push(output.download(&mut gpu.context)?);
    }
    assert!(histories.windows(2).all(|pair| pair[0] == pair[1]));
    assert_ne!(outputs[0], outputs[2], "输出端点实际选择不同估计");
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；历史cap在实际temporal MIS中限制累积质量"]
fn hardware_pt_restir_history_cap_limits_temporal_mass() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    let mut masses = Vec::new();
    for cap in [1, 100] {
        gpu.clear_world()?;
        gpu.set_rendering(
            prime_scene::RenderMode::Realtime,
            RealtimeRenderer::RestirPt,
        )?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(cap, 0., false, false)?)?;
        publish(&mut gpu, &mesh, 1)?;
        for _ in 0..8 {
            host.render_options(
                &mut gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, 1, 6)?,
            )?;
        }
        let history = gpu.restir.as_ref().unwrap().history_buffer();
        let bytes = download_buffer(&mut gpu, history)?;
        let stride = size_of::<simple_engine::restir_pt::PathReservoir>();
        let maximum = bytes
            .chunks_exact(stride)
            .map(|record| f32::from_le_bytes(record[..4].try_into().unwrap()))
            .fold(0f32, f32::max);
        masses.push(maximum);
    }
    assert!(
        masses[0] <= 8. && masses[1] > 8.,
        "三邻居合并后的质量 {masses:?}"
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；ReSTIR完整阶段、历史和返回普通PT后的工作集回收"]
fn hardware_pt_restir_switch_and_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::RestirPt,
    )?;
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }],
    })?;
    for _ in 0..3 {
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            Options::new(false, streamline::Quality::Performance, 1, 6)?,
        )?;
    }
    let renderer = gpu.restir.as_mut().unwrap();
    let frame = renderer.frame()?;
    assert_eq!(frame.frame_index, 3);
    assert!(frame.history_valid);
    let mut hdr = vec![0; 32 * 32 * 16];
    let readback = Buffer::new(&mut gpu.context, hdr.len() as u64, true)?;
    gpu.view.as_ref().unwrap().output.copy_prefix_to(
        &mut gpu.context,
        &readback,
        hdr.len() as u64,
    )?;
    readback.read(&mut gpu.context, 0, &mut hdr)?;
    assert!(
        hdr.as_chunks::<4>()
            .0
            .iter()
            .all(|v| f32::from_le_bytes(*v).is_finite())
    );
    assert!(
        hdr.as_chunks::<16>()
            .0
            .iter()
            .any(|v| f32::from_le_bytes(v[..4].try_into().unwrap()) > 0.)
    );
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::PathTrace,
    )?;
    assert!(gpu.restir.is_none() && gpu.restir_geometry.is_none());
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Performance, 2, 6)?,
    )?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 2);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan EXT SER；相同ReSTIR帧序列在重排开关下保持HDR结果"]
fn hardware_pt_restir_ser_preserves_radiance() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    let mut reference: Option<Vec<u8>> = None;
    for enabled in [false, true] {
        gpu.clear_world()?;
        gpu.set_ser(enabled)?;
        gpu.set_rendering(
            prime_scene::RenderMode::Realtime,
            RealtimeRenderer::RestirPt,
        )?;
        publish(&mut gpu, &mesh, 1)?;
        for _ in 0..3 {
            host.render_options(
                &mut gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, 1, 6)?,
            )?;
        }
        let hdr = gpu.view.as_ref().unwrap().output;
        let readback = Buffer::new(&mut gpu.context, hdr.len(), true)?;
        hdr.copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
        let mut bytes = vec![0; hdr.len() as usize];
        readback.read(&mut gpu.context, 0, &mut bytes)?;
        if let Some(reference) = &reference {
            for (a, b) in reference
                .as_chunks::<4>()
                .0
                .iter()
                .zip(bytes.as_chunks::<4>().0)
            {
                let a = f32::from_le_bytes(*a);
                let b = f32::from_le_bytes(*b);
                assert!(a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-6 * a.abs().max(1.));
            }
        } else {
            reference = Some(bytes);
        }
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；时空复用均值与独立PT的同场景辐亮度对照"]
fn hardware_pt_restir_mean_matches_independent_paths() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    publish(&mut gpu, &mesh, 1)?;
    for _ in 0..8 {
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            Options::new(false, streamline::Quality::Dlaa, 64, 6)?,
        )?;
    }
    let expected = hdr_mean(&mut gpu)?;
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::RestirPt,
    )?;
    assert!(!gpu.try_attribute_changes(gpu.resource_generation, [0.; 3], &[])?);
    publish(&mut gpu, &mesh, 1)?;
    let mut total = [0.; 3];
    for _ in 0..64 {
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            Options::new(false, streamline::Quality::Dlaa, 1, 6)?,
        )?;
        for (sum, value) in total.iter_mut().zip(hdr_mean(&mut gpu)?) {
            *sum += value;
        }
    }
    for channel in 0..3 {
        let actual = total[channel] / 64.;
        assert!(expected[channel] > 0.);
        let ratio = actual / expected[channel];
        eprintln!(
            "ReSTIR channel={channel} mean={actual} reference={} ratio={ratio}",
            expected[channel]
        );
        assert!(
            (0.8..1.2).contains(&ratio),
            "复用均值超出固定采样容差：{ratio}"
        );
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；纯属性发布更新当前颜色并保留路径历史"]
fn hardware_pt_restir_content_change_preserves_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::RestirPt,
    )?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    publish(&mut gpu, &mesh, 1)?;
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 6)?;
    for _ in 0..3 {
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            options,
        )?;
    }
    mesh.quads.clear();
    add_quad(&mut mesh, 2., [0.5; 2], [0.05, 0.8, 0.05], 4.);
    publish(&mut gpu, &mesh, 2)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options,
    )?;
    let restir = gpu.restir.as_mut().unwrap();
    assert_eq!(restir.frame()?.frame_index, 4);
    let color = hdr_mean(&mut gpu)?;
    assert!(
        color[1] > 2. * color[0] && color[1] > 2. * color[2],
        "新颜色没有独立发布：{color:?}"
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；离线继承曝光且只在调节强度变化时重测"]
fn hardware_pt_offline_preserves_exposure() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    gpu.set_display_options(0.65, 0.05, 0.75, 0.4)?;
    let live = Options::new(false, streamline::Quality::Dlaa, 1, 6)?;
    host.render_options(&mut gpu, &output, &camera(), Environment::default(), live)?;
    let inherited = exposure_state(&mut gpu)?;
    gpu.set_rendering(prime_scene::RenderMode::Offline, RealtimeRenderer::RestirPt)?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独立进程：真实SDK配置和同尺寸视图替换只冷启动RR，保留路径历史；不evaluate"]
fn hardware_pt_rr_restir_storage_preserves_path_history() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new_rr()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::RestirPt,
    )?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [0.8; 3], 8.);
    publish(&mut gpu, &mesh, 1)?;
    gpu.rr_quality = streamline::Quality::Dlaa;
    gpu.settings.samples = 1;
    for (frame, rr) in [false, true, false].into_iter().enumerate() {
        let history = gpu.restir.as_ref().map(|r| r.history_buffer());
        let records = gpu.record_frame(
            [32, 32],
            &camera(),
            Environment::default(),
            Binding::StorageImage(output),
            rr,
        )?;
        for record in records {
            record.submit_and_wait(&mut gpu.context)?;
        }
        gpu.complete_previous()?;
        let restir = gpu.restir.as_ref().unwrap();
        assert_eq!(restir.frame()?.frame_index, frame as u32 + 1);
        if let Some(history) = history {
            assert_eq!(history, restir.history_buffer());
        }
    }
    assert_eq!(gpu.history_audit.restir_storage, 1);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT和Streamline配置；静止相机跨Halton周期的真实shader donor不漂移，不执行SDK evaluate"]
fn hardware_pt_rr_restir_jitter_donor_is_stationary() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr()?;
    let mut gpu = host.gpu()?;
    gpu.rr_quality = streamline::Quality::Dlaa;
    gpu.settings.samples = 1;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [0.8; 3], 8.);
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::RestirPt,
    )?;
    publish(&mut gpu, &mesh, 1)?;
    let donors = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.test_roots.push(donors.into());
    let mut probe = None;
    for index in 0..32 {
        gpu.rr_index = index + 11;
        let records = gpu.record_frame(
            [32, 32],
            &camera(),
            Environment::default(),
            Binding::StorageImage(output),
            true,
        )?;
        for record in records {
            record.submit_and_wait(&mut gpu.context)?;
        }
        gpu.complete_previous()?;
        gpu.atmosphere.submitted();
        assert_eq!(gpu.view.as_ref().unwrap().extent, [32, 32]);
        if probe.is_none() {
            let (scene, _) = gpu.renderer_bindings(
                gpu.view.as_ref().unwrap(),
                &gpu.atlas,
                &gpu.scene,
                gpu.material_gpu,
                gpu.lighting_gpu,
            );
            let pipeline = gpu.restir.as_ref().unwrap().reprojection_probe(
                &mut gpu.context,
                scene,
                gpu.restir_geometry.unwrap(),
                gpu.restir_lut,
                donors,
            )?;
            gpu.test_roots.push(pipeline.into());
            probe = Some(pipeline);
        }
        if index == 0 {
            continue;
        }
        let mut commands = Commands::new(&gpu.context)?;
        commands.dispatch(probe.unwrap(), [2, 2, 1])?;
        commands
            .finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        let mut bytes = vec![0; 32 * 32 * 16];
        donors.read(&mut gpu.context, 0, &mut bytes)?;
        for pixel in [[4usize, 8usize], [27, 19], [20, 4]] {
            let start = (pixel[1] * 32 + pixel[0]) * 16;
            let actual = std::array::from_fn(|axis| {
                u32::from_le_bytes(
                    bytes[start + axis * 4..start + axis * 4 + 4]
                        .try_into()
                        .unwrap(),
                )
            });
            assert_eq!(
                pixel.map(|v| v as u32),
                actual,
                "Halton帧 {index} 的静止donor；history/primary={:?}",
                &bytes[start + 8..start + 16]
            );
        }
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT和Streamline；ReSTIR生成真实导引并完成RR"]
fn hardware_pt_rr_restir_guides() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        RealtimeRenderer::RestirPt,
    )?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    publish(&mut gpu, &mesh, 1)?;
    for _ in 0..2 {
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            Options::new(true, streamline::Quality::Dlaa, 1, 6)?,
        )?;
    }
    assert!(gpu.rr_unavailable.is_none());
    let view = gpu.view.as_ref().unwrap();
    assert!(view.rr_enabled && gpu.rr_previous.is_some());
    let depth = view.rr.images[1].download(&mut gpu.context)?;
    let values = depth.as_chunks::<4>().0;
    assert!(
        values
            .iter()
            .all(|word| f32::from_le_bytes(*word).is_finite())
    );
    assert!(f32::from_le_bytes(values[16 * 32 + 16]) > 0.);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
