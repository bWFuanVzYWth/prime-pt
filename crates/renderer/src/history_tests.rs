//! 真实GPU工作集验证局部保留、显式失效、取消和槽复用；不启动游戏。
use super::*;
use prime_scene::{RealtimeRenderer, RenderMode, RenderRegion, RenderView};

fn options() -> Result<Options, Error> {
    Options::new(false, streamline::Quality::Dlaa, 1, 6)
}

fn publish(gpu: &mut Gpu, mesh: &Mesh, anchor: [f64; 3]) -> Result<(), Error> {
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor,
        regions: &[RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: mesh.into(),
        }],
    })
}

fn setup(gpu: &mut Gpu) -> Result<Mesh, Error> {
    gpu.set_rendering(RenderMode::Realtime, RealtimeRenderer::RestirPt)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [0.8; 3], 8.);
    publish(gpu, &mesh, [0.; 3])?;
    Ok(mesh)
}

fn bytes(gpu: &mut Gpu, buffer: Buffer) -> Result<Vec<u8>, Error> {
    let readback = Buffer::new(&mut gpu.context, buffer.len(), true)?;
    buffer.copy_prefix_to(&mut gpu.context, &readback, buffer.len())?;
    let mut result = vec![0; buffer.len() as usize];
    readback.read(&mut gpu.context, 0, &mut result)?;
    Ok(result)
}

#[test]
#[ignore = "真实Vulkan PT；输入改变显式清除静态均值，不能触及重投影消费者"]
fn hardware_pt_static_mean_uses_explicit_input_event() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [0.8; 3], 8.);
    publish(&mut gpu, &mesh, [0.; 3])?;
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 6)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options,
    )?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options,
    )?;
    assert_eq!(gpu.accumulated_samples(), 4);
    let resets = [gpu.history_audit.rr_resets, gpu.history_audit.restir_resets];
    let mut camera = camera();
    camera.origin[0] += 0.125;
    host.render_options(&mut gpu, &output, &camera, Environment::default(), options)?;
    assert_eq!(gpu.accumulated_samples(), 2);
    assert_eq!(
        gpu.history_audit.last_accumulation,
        Some(history::AccumulationEvent::FrameInputsChanged)
    );
    let counts = gpu.view.as_ref().unwrap().counts;
    assert!(
        bytes(&mut gpu, counts)?
            .as_chunks::<4>()
            .0
            .iter()
            .all(|v| u32::from_le_bytes(*v) == 2)
    );
    assert_eq!(
        [gpu.history_audit.rr_resets, gpu.history_audit.restir_resets],
        resets
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；日照/相机/FOV/anchor/介质/随机与预算变化不能全局重置"]
fn hardware_pt_restir_history_survives_frame_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mesh = setup(&mut gpu)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let resets = [gpu.history_audit.rr_resets, gpu.history_audit.restir_resets];
    let storage = gpu.history_audit.restir_storage;
    let mut camera = camera();
    for frame in 1..=8 {
        let angle = frame as f32 * 0.0001;
        let environment = Environment {
            sun_direction: [angle.sin(), angle.cos(), 0.],
            ..Environment::default()
        };
        camera.right[0] *= 1.001;
        gpu.settings.seed += 1;
        gpu.set_initial_medium([1. + angle, 0., 0., 0.])?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(20 + frame, 0.4, false)?)?;
        if frame == 4 {
            publish(&mut gpu, &mesh, [64., 0., 0.])?;
            camera.origin[0] -= 64.;
        }
        host.render_options(
            &mut gpu,
            &output,
            &camera,
            environment,
            Options::new(false, streamline::Quality::Dlaa, 1, 6 + frame % 3)?,
        )?;
        let state = gpu.restir.as_ref().unwrap().frame()?;
        assert_eq!(state.frame_index, frame + 1);
        assert!(state.history_valid);
    }
    assert_eq!(
        [gpu.history_audit.rr_resets, gpu.history_audit.restir_resets],
        resets
    );
    assert_eq!(gpu.history_audit.restir_storage, storage);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；RGB重启和封闭全局事件分别影响各自消费者，不回退绝对采样"]
fn hardware_pt_history_explicit_reset_scope() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    setup(&mut gpu)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    // 此夹具只验证RR已接受元数据的失效范围，不声称执行SDK evaluate。
    gpu.rr_previous = Some(CameraFrame::new(&camera(), gpu.anchor)?);
    gpu.rr_index = 17;
    gpu.rr_sample = 33;
    let state = gpu.restir.as_ref().unwrap().frame()?;
    let resets = [gpu.history_audit.rr_resets, gpu.history_audit.restir_resets];
    let exposure = gpu.view.as_ref().unwrap().display.state();
    let accepted_exposure = bytes(&mut gpu, exposure)?;
    gpu.restart_accumulation()?;
    assert!(gpu.rr_previous.is_some());
    assert_eq!(gpu.restir.as_ref().unwrap().frame()?, state);
    assert_eq!(
        [gpu.history_audit.rr_resets, gpu.history_audit.restir_resets],
        resets
    );
    for reason in [
        GlobalReset::WorldReplaced,
        GlobalReset::SceneDomainReplaced,
        GlobalReset::RenderDomainChanged,
        GlobalReset::ExplicitRequested,
    ] {
        gpu.invalidate_history(reason)?;
        assert_eq!(gpu.accumulated_samples(), 0);
        assert!(gpu.rr_previous.is_none());
        let restir = gpu.restir.as_ref().unwrap();
        assert!(!restir.frame()?.history_valid);
        assert_eq!(restir.frame()?.frame_index, 0);
        assert!(restir.previous_camera.is_none());
        assert_eq!((gpu.rr_index, gpu.rr_sample), (17, 33));
        assert_eq!(bytes(&mut gpu, exposure)?, accepted_exposure);
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            options()?,
        )?;
        gpu.rr_previous = Some(CameraFrame::new(&camera(), gpu.anchor)?);
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT/SER；已准备程序切换不替换路径工作集或重开历史"]
fn hardware_pt_restir_history_survives_ser_switch() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    setup(&mut gpu)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let history = gpu.restir.as_ref().unwrap().history_buffer();
    let reset_count = gpu.history_audit.restir_resets;
    let storage_count = gpu.history_audit.restir_storage;
    for frame in 2..=4 {
        gpu.set_ser(!gpu.invocation_reorder)?;
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            options()?,
        )?;
        let restir = gpu.restir.as_ref().unwrap();
        assert_eq!(restir.history_buffer(), history);
        assert_eq!(restir.frame()?.frame_index, frame);
        assert!(restir.frame()?.history_valid);
    }
    assert_eq!(gpu.history_audit.restir_resets, reset_count);
    assert_eq!(gpu.history_audit.restir_storage, storage_count);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn probe_identity(gpu: &mut Gpu, hit: [u32; 4]) -> Result<[u32; 4], Box<dyn std::error::Error>> {
    let output = Buffer::upload(&mut gpu.context, bytemuck::cast_slice(&[hit, [0; 4]]))?;
    let view = gpu.view.as_ref().unwrap();
    let (scene, _) = gpu.renderer_bindings(
        view,
        &gpu.atlas,
        &gpu.scene,
        gpu.material_gpu,
        gpu.lighting_gpu,
    );
    let probe = gpu.restir.as_ref().unwrap().identity_probe(
        &mut gpu.context,
        scene,
        gpu.restir_geometry.unwrap(),
        gpu.restir_lut,
        output,
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.dispatch(probe, [1, 1, 1])?;
    commands.barrier();
    let commands = commands.finish(&mut gpu.context)?;
    commands.submit_and_wait(&mut gpu.context)?;
    let data = bytes(gpu, output)?;
    Ok(std::array::from_fn(|i| {
        u32::from_le_bytes(data[16 + i * 4..20 + i * 4].try_into().unwrap())
    }))
}

#[test]
#[ignore = "真实shader：删除和属性槽复用不能使旧mesh身份复活，其余全屏历史继续"]
fn hardware_pt_restir_identity_rejects_removed_and_reused_slots()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mesh = setup(&mut gpu)?;
    let both = |revision| {
        [
            RenderRegion {
                key: [0; 3],
                geometry_revision: revision,
                content_revision: revision,
                mesh: (&mesh).into(),
            },
            RenderRegion {
                key: [1, 0, 0],
                geometry_revision: 1,
                content_revision: 1,
                mesh: (&mesh).into(),
            },
        ]
    };
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &both(1),
    })?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let range = gpu.region_cache.entries()[0].range.clone();
    let identity = gpu.region_cache.entries()[0].identity;
    let primary = gpu.restir.as_ref().unwrap().primary_history();
    let primary = bytes(&mut gpu, primary)?;
    let old = primary
        .as_chunks::<16>()
        .0
        .iter()
        .map(|b| {
            std::array::from_fn::<_, 4, _>(|i| {
                u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap())
            })
        })
        .find(|hit| hit[0] != 0 && range.contains(&((hit[0] - 1) / 2)) && hit[3] == identity)
        .ok_or("没有命中待删除mesh")?;
    assert_eq!(probe_identity(&mut gpu, old)?[..3], [1; 3]);
    let reset_count = gpu.history_audit.restir_resets;
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &both(1)[1..],
    })?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    assert_eq!(probe_identity(&mut gpu, old)?[..3], [0; 3]);
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &both(2),
    })?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let current = &gpu.region_cache.entries()[0];
    assert_eq!(current.range, range, "夹具必须实际复用旧属性槽");
    assert_ne!(current.identity, identity);
    let next_identity = current.identity;
    assert_eq!(probe_identity(&mut gpu, old)?[..3], [0; 3]);
    let mut fresh = old;
    fresh[3] = next_identity;
    assert_eq!(probe_identity(&mut gpu, fresh)?[..3], [1; 3]);
    assert_eq!(gpu.restir.as_ref().unwrap().frame()?.frame_index, 3);
    assert_eq!(gpu.history_audit.restir_resets, reset_count);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实shader：稳定身份不能破坏着色法线，和PT命中解码对照"]
fn hardware_pt_restir_stable_hit_preserves_authored_normal()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut definition = prime_scene::MaterialDefinition::default();
    definition.control[2] = prime_scene::MaterialDefinition::AUXILIARY;
    definition.control[3] = prime_scene::MaterialDefinition::AUTHORED_NORMAL;
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [1, 1],
        albedo: &[255; 4],
        materials: &[definition],
        mers: &[0, 255, 242, 0],
        auxiliary: &[192, 128, 0, 85],
        emission_alias_words: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]],
    })?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [0.8; 3], 8.);
    mesh.quads[0] = Quad::with_material(
        mesh.quads[0].positions(),
        [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
        [0.8; 3],
        prime_scene::MaterialId::new(0),
    );
    publish(&mut gpu, &mesh, [0.; 3])?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let landing = gpu.view.as_ref().unwrap().landing[5];
    let reference = bytes(&mut gpu, landing)?;
    let reference = reference
        .as_chunks::<16>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(v[12..16].try_into().unwrap()))
        .find(|&n| n != 0)
        .ok_or("PT没有记录作者法线")?;
    gpu.set_rendering(RenderMode::Realtime, RealtimeRenderer::RestirPt)?;
    publish(&mut gpu, &mesh, [0.; 3])?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let history = gpu.restir.as_ref().unwrap().primary_history();
    let hits = bytes(&mut gpu, history)?;
    let hit = hits
        .as_chunks::<16>()
        .0
        .iter()
        .map(|v| {
            std::array::from_fn::<_, 4, _>(|i| {
                u32::from_le_bytes(v[4 * i..4 * i + 4].try_into().unwrap())
            })
        })
        .find(|h| h[0] != 0 && h[3] != 0)
        .ok_or("ReSTIR没有命中")?;
    assert_eq!(probe_identity(&mut gpu, hit)?[3], reference);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实录制与取消：不提交的帧不能抹掉上次已接受路径/相机"]
fn hardware_pt_restir_cancel_preserves_accepted_history() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    setup(&mut gpu)?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    let state = gpu.restir.as_ref().unwrap().frame()?;
    let mut moved = camera();
    moved.origin[0] = 0.25;
    let records = gpu.record_frame(
        [32, 32],
        &moved,
        Environment::default(),
        Binding::StorageImage(output),
        false,
    )?;
    let resets = gpu.history_audit.restir_resets;
    assert!(
        gpu.transition_history(history::Event::Global(GlobalReset::ExplicitRequested))
            .is_err()
    );
    assert_eq!(
        gpu.history_audit.restir_resets, resets,
        "待接受帧拒绝重置且不改变消费者"
    );
    gpu.cancel_recordings(records)?;
    gpu.transition_history(history::Event::Accumulation(
        history::AccumulationEvent::RecordingCancelled,
    ))?;
    assert_eq!(gpu.restir.as_ref().unwrap().frame()?, state);
    assert!(gpu.restir.as_ref().unwrap().previous_camera.is_some());
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        options()?,
    )?;
    assert_eq!(gpu.restir.as_ref().unwrap().frame()?.frame_index, 2);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
