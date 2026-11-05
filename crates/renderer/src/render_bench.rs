//! 真实 draw 入口的 CPU 录制与 GPU 全帧跨度；固定场景，不代表 Minecraft 帧率。
use super::*;
use prime_scene::{RealtimeRenderer, RenderMode, RenderRegion, RenderView};

#[test]
#[ignore = "独占Vulkan RT/SER设备；PRIME_PT_RENDER_BENCH_OUTPUT须指向新的JSON文件"]
fn hardware_pt_render_profile() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var_os("PRIME_PT_RENDER_BENCH_OUTPUT")
        .ok_or("缺少PRIME_PT_RENDER_BENCH_OUTPUT")?;
    let host = Host::new_ser()?;
    let mut gpu = host.gpu()?;
    let output = Image::new(&mut gpu.context, [960, 540, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let queries = [
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
    ];
    gpu.test_roots
        .extend(queries.map(vulkan::native::Resource::from));
    let mut mesh = Mesh::default();
    for y in 0..64 {
        for x in 0..64 {
            let x = x as f32 * 0.5 - 16.;
            let y = y as f32 * 0.5 - 16.;
            mesh.quads.push(Quad::new(
                [
                    [x, y, 8.],
                    [x, y + 0.5, 8.],
                    [x + 0.5, y + 0.5, 8.],
                    [x + 0.5, y, 8.],
                ],
                [[0.5; 2]; 4],
                [0.8, 0.6, 0.4],
                Material::Opaque,
            ));
        }
    }
    let mut cases = Vec::new();
    for renderer in [RealtimeRenderer::PathTrace, RealtimeRenderer::RestirPt] {
        gpu.set_rendering(RenderMode::Realtime, renderer)?;
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
        let mut samples = Vec::new();
        let mut frame = 0;
        let warmup = std::time::Instant::now();
        while samples.len() < 64 {
            if renderer == RealtimeRenderer::PathTrace {
                gpu.transition_history(history::Event::Accumulation(
                    history::AccumulationEvent::ExplicitRequested,
                ))?;
            }
            let timing = host.render_profiled(
                &mut gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, 1, 12)?,
                Some(queries),
            )?;
            if frame >= 64 && warmup.elapsed() >= std::time::Duration::from_secs(2) {
                samples.push(timing);
            }
            frame += 1;
        }
        let pixels = output.download(&mut gpu.context)?;
        assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[..3] != [0; 3]));
        cases.push(
            serde_json::json!({"renderer":format!("{renderer:?}"), "cpu_gpu_ns":samples, "frames":frame,
            "display_blake3":blake3::hash(&pixels).to_hex().as_str()}),
        );
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(
        file,
        &serde_json::json!({
            "extent":[960,540], "quads":4096, "samples":1, "bounces":12, "ser":true, "rr":false,
            "warmup_ms":2000,
            "scope":"CPU draw录制含回收；GPU timestamp包围完整draw；不含Java、场景更新与窗口呈现",
            "cases":cases
        }),
    )?;
    Ok(())
}
