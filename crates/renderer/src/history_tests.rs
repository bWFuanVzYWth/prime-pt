//! 真实GPU工作集验证局部保留、显式失效、取消和槽复用；不启动游戏。
use super::*;
use prime_scene::{RenderRegion, RenderView};

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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let resets = gpu.history_audit.rr_resets;
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
    assert_eq!(gpu.history_audit.rr_resets, resets);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
