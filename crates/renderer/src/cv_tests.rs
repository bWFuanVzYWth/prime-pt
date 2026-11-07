//! 生产ReSTCV的颜色事务、路径独立性和GPU组合公式；不启动客户端。
use super::*;
use prime_scene::{RealtimeRenderer, RenderMode, RenderTuning};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn tuning(cv: bool) -> RenderTuning {
    RenderTuning::new(5, 0., false, cv).unwrap()
}

fn setup(gpu: &mut Gpu) -> Result {
    gpu.set_rendering(RenderMode::Realtime, RealtimeRenderer::RestirPt)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 4., [0.5; 2], [0.8; 3], 8.);
    let start = mesh.quads.len();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 0.5);
    for quad in &mut mesh.quads[start..] {
        quad.translate([0.5, 0.5, 0.]);
    }
    gpu.set_regions(prime_scene::RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[prime_scene::RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }],
    })?;
    Ok(())
}

fn render(host: &Host, gpu: &mut Gpu, output: &Image) -> Result {
    host.render_options(
        gpu,
        output,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Dlaa, 1, 6)?,
    )
}

fn read(gpu: &mut Gpu, source: Buffer) -> Result<Vec<u8>> {
    let target = Buffer::new(&mut gpu.context, source.len(), true)?;
    source.copy_prefix_to(&mut gpu.context, &target, source.len())?;
    let mut bytes = vec![0; source.len() as usize];
    target.read(&mut gpu.context, 0, &mut bytes)?;
    Ok(bytes)
}

fn write(gpu: &mut Gpu, target: Buffer, bytes: &[u8]) -> Result {
    let source = Buffer::upload(&mut gpu.context, bytes)?;
    source.copy_prefix_to(&mut gpu.context, &target, bytes.len() as u64)?;
    Ok(())
}

fn floats(gpu: &mut Gpu, source: Buffer) -> Result<Vec<f32>> {
    Ok(read(gpu, source)?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect())
}

fn binding(gpu: &mut Gpu, output: Option<Buffer>) -> Result<Bindings> {
    let (scene, _) = gpu.renderer_bindings(
        gpu.view.as_ref().unwrap(),
        &gpu.atlas,
        &gpu.scene,
        gpu.material_gpu,
        gpu.lighting_gpu,
    );
    Ok(gpu.restir.as_ref().unwrap().test_bindings(
        &mut gpu.context,
        scene,
        gpu.restir_geometry.unwrap(),
        gpu.restir_lut,
        output,
    )?)
}

#[test]
#[ignore = "真实RT；CV只改变颜色估计，逐帧路径选样保持原位字节相同"]
fn hardware_pt_restcv_preserves_path_sampling() -> Result {
    let host = Host::new()?;
    let mut reference = Vec::new();
    let mut changed = false;
    for comparison in [false, true] {
        let mut session = host.gpu()?;
        let mut gpu = session.gpu();
        let output = Image::new(&mut gpu.context, [31, 17, 1], ImageFormat::Rgba8Unorm)?;
        gpu.test_roots.push(output.into());
        setup(&mut gpu)?;
        for (frame, enabled) in [false, true, true, false, true, true]
            .into_iter()
            .enumerate()
        {
            gpu.set_render_tuning(tuning(comparison && enabled))?;
            render(&host, &mut gpu, &output)?;
            let buffers = gpu.restir.as_ref().unwrap().test_buffers();
            let paths = *buffers.reservoirs();
            let colors = *buffers.color_estimates().unwrap();
            let actual = [read(&mut gpu, paths[0])?, read(&mut gpu, paths[1])?];
            if comparison {
                assert_eq!(reference[frame], actual, "CV不得改变路径或随机流");
                if enabled {
                    let color = floats(&mut gpu, colors[0])?;
                    for (path, rgb) in actual[0]
                        .as_chunks::<80>()
                        .0
                        .iter()
                        .zip(color.as_chunks::<3>().0)
                    {
                        let weight = f32::from_le_bytes(path[4..8].try_into()?);
                        for c in 0..3 {
                            let value = f32::from_le_bytes(path[8 + 4 * c..12 + 4 * c].try_into()?);
                            assert!(rgb[c].is_finite());
                            changed |= (rgb[c] - weight * value).abs() > 1e-6;
                        }
                    }
                }
            } else {
                reference.push(actual);
            }
        }
    }
    assert!(changed, "颜色估计须独立于temporal选中的路径");
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实RT；颜色只由成功帧接受，开关/取消/SER均不能隐式清除路径"]
fn hardware_pt_restcv_color_history_is_explicit() -> Result {
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    setup(&mut gpu)?;
    let mut storage = None;
    for (frame, (enabled, old_color)) in [
        (true, false),
        (true, true),
        (false, false),
        (true, false),
        (true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let resets = gpu.history_audit.restir_resets;
        gpu.set_render_tuning(tuning(enabled))?;
        assert_eq!(gpu.history_audit.restir_resets, resets);
        if frame == 4 {
            gpu.set_ser(false)?;
        }
        render(&host, &mut gpu, &output)?;
        let restir = gpu.restir.as_ref().unwrap();
        assert_eq!(restir.frame()?.frame_index, frame as u32 + 1);
        assert_eq!(restir.color_history_valid(), enabled);
        let colors = *restir.test_buffers().color_estimates().unwrap();
        let paths = restir.history_buffer();
        let parameter_buffer = gpu.view.as_ref().unwrap().params;
        let params = floats(&mut gpu, parameter_buffer)?;
        // 尾部四个16B记录中的首个uint4为输出域。
        let flags = params[params.len() - 14].to_bits();
        assert_eq!(flags & 16 != 0, old_color);
        if let Some(expected) = storage {
            assert_eq!(colors, expected);
        } else {
            storage = Some(colors);
        }
        if enabled {
            let accepted = [read(&mut gpu, colors[1])?, read(&mut gpu, paths)?];
            let state = gpu.restir.as_ref().unwrap().frame()?;
            let records = gpu.record_frame(
                [32, 32],
                &camera(),
                Environment::default(),
                Binding::StorageImage(output),
                false,
            )?;
            assert!(
                gpu.transition_history(history::Event::RestirColorModeChanged)
                    .is_err()
            );
            gpu.cancel_recordings(records)?;
            assert_eq!(gpu.restir.as_ref().unwrap().frame()?, state);
            assert!(gpu.restir.as_ref().unwrap().color_history_valid());
            assert_eq!(
                [read(&mut gpu, colors[1])?, read(&mut gpu, paths)?],
                accepted
            );
        }
    }
    gpu.invalidate()?;
    assert!(
        gpu.restir.as_ref().unwrap().color_history_valid(),
        "RGB重启保留CV历史"
    );
    gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
    assert!(!gpu.restir.as_ref().unwrap().color_history_valid());
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "EXT SER；CV启用时同一帧序列的颜色与HDR保持相同估计"]
fn hardware_pt_restcv_ser_preserves_color_estimate() -> Result {
    let host = Host::new_ser()?;
    let mut reference = Vec::new();
    for reordered in [false, true] {
        let mut session = host.gpu()?;
        let mut gpu = session.gpu();
        gpu.set_ser(reordered)?;
        let output = image(&mut gpu)?;
        setup(&mut gpu)?;
        gpu.set_render_tuning(tuning(true))?;
        for frame in 0..4 {
            render(&host, &mut gpu, &output)?;
            let restir = gpu.restir.as_ref().unwrap();
            let colors = *restir.test_buffers().color_estimates().unwrap();
            let hdr = gpu.view.as_ref().unwrap().output;
            let actual = [
                floats(&mut gpu, colors[0])?,
                floats(&mut gpu, colors[1])?,
                floats(&mut gpu, hdr)?,
            ];
            if reordered {
                let expected: &[Vec<f32>; 3] = &reference[frame];
                for (a, b) in actual.iter().flatten().zip(expected.iter().flatten()) {
                    assert!(a.is_finite() && b.is_finite());
                    assert!((a - b).abs() <= 1e-5 * (1. + a.abs().max(b.abs())));
                }
            } else {
                reference.push(actual);
            }
        }
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

// f64直接按两源measure的partition-of-unity计算，不使用GPU的缩放实现。
fn difference(c: &[f32; 20], n: &[f32; 20], forward: &[f32; 5], reverse: &[f32; 5]) -> [f64; 3] {
    let mapped = |s: &[f32; 5]| -> [f64; 3] {
        std::array::from_fn(|i| {
            if s[3] > 0. {
                f64::from(s[i]) * f64::from(s[3])
            } else {
                0.
            }
        })
    };
    let target = |v: [f64; 3]| 0.299 * v[0] + 0.587 * v[1] + 0.114 * v[2];
    let pair = |s: &[f32; 20], other: f32, mapped| {
        let p = target([f64::from(s[2]), f64::from(s[3]), f64::from(s[4])]);
        let q = target(mapped);
        if p <= 0. || s[1] <= 0. {
            0.
        } else if q <= 0. {
            1.
        } else {
            p * f64::from(s[0]) / (p * f64::from(s[0]) + q * f64::from(other))
        }
    };
    let f = mapped(forward);
    let r = mapped(reverse);
    let a = pair(c, n[0], r) * f64::from(c[1]);
    let b = pair(n, c[0], f) * f64::from(n[1]);
    std::array::from_fn(|i| a * (f64::from(c[2 + i]) - r[i]) + b * (f[i] - f64::from(n[2 + i])))
}

#[test]
#[ignore = "生产temporal compute；独立f64公式验证双向shift、M cap和有符号颜色"]
fn hardware_pt_restcv_temporal_matches_measure_formula() -> Result {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    setup(&mut gpu)?;
    gpu.set_render_tuning(RenderTuning::new(37, 0., false, true)?)?;
    render(&host, &mut gpu, &output)?;
    render(&host, &mut gpu, &output)?;
    let buffers = gpu.restir.as_ref().unwrap().test_buffers();
    let paths = *buffers.reservoirs();
    let colors = *buffers.color_estimates().unwrap();
    let mask = *buffers.neighbor_mask();
    let shifts = *buffers.precomputed();
    let n = buffers.layout().elements() as usize;
    let mut canonical = [0f32; 20];
    canonical[..5].copy_from_slice(&[1., 0.75, 2., 3., 5.]);
    let mut neighbor = [0f32; 20];
    neighbor[..5].copy_from_slice(&[50., 1.25, 4., 2., 1.]);
    let mut a = vec![[0f32; 20]; n];
    a[0] = canonical;
    let mut b = vec![[0f32; 20]; n];
    b[1] = neighbor;
    write(&mut gpu, paths[0], bytemuck::cast_slice(&a))?;
    write(&mut gpu, paths[1], bytemuck::cast_slice(&b))?;
    let old = [-2f32, 1., 0.5];
    let mut rgb = vec![[0f32; 3]; n];
    rgb[1] = old;
    write(&mut gpu, colors[1], bytemuck::cast_slice(&rgb))?;
    let mut masks = vec![[0u32; 2]; n];
    masks[0] = [1, 1];
    write(&mut gpu, mask, bytemuck::cast_slice(&masks))?;
    let forward = [1f32, 4., 0., 0.75, 1.];
    let reverse = [3f32, 0., 2., 0.5, 1.];
    let mut cache = vec![[0f32; 5]; 3 * n];
    cache[0] = forward;
    cache[1] = reverse;
    write(&mut gpu, shifts, bytemuck::cast_slice(&cache))?;
    let bindings = binding(&mut gpu, None)?;
    let pipeline = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/restir-temporal-combine.spv")),
            c"restir_temporal_combine",
        ),
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.dispatch(pipeline, [2, 2, 1])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    neighbor[0] = 37.;
    let delta = difference(&canonical, &neighbor, &reverse, &forward);
    let actual = floats(&mut gpu, colors[0])?;
    let mut negative = false;
    for c in 0..3 {
        let expected = (f64::from(canonical[1] * canonical[2 + c])
            + 37. * (f64::from(old[c]) + delta[c]))
            / 38.;
        assert!(
            (f64::from(actual[c]) - expected).abs() <= 1e-5 * expected.abs().max(1.),
            "{actual:?} expected={expected}"
        );
        negative |= actual[c] < 0.;
    }
    assert!(negative, "合法负校正必须保留");
    assert_eq!(floats(&mut gpu, paths[0])?[0], 38.);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "生产spatial merge/post；f64公式核对paired双向缓存和有符号线性输出"]
fn hardware_pt_restcv_spatial_matches_measure_formula() -> Result {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    setup(&mut gpu)?;
    gpu.set_render_tuning(tuning(true))?;
    render(&host, &mut gpu, &output)?;
    render(&host, &mut gpu, &output)?;
    let buffers = gpu.restir.as_ref().unwrap().test_buffers();
    let path = buffers.reservoirs()[0];
    let colors = *buffers.color_estimates().unwrap();
    let shifts = *buffers.precomputed();
    let n = buffers.layout().elements() as usize;
    let indices = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    let bindings = binding(&mut gpu, Some(indices))?;
    let probe = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/restir_cv_neighbors.spv")),
            c"restir_cv_neighbors",
        ),
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.dispatch(probe, [2, 2, 1])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let indices = read(&mut gpu, indices)?;
    let paths: Vec<[f32; 20]> = (0..n)
        .map(|i| {
            let mut value = [0.; 20];
            value[..5].copy_from_slice(&[1., 1., 1. + (i % 7) as f32, 0.1 + (i % 3) as f32, 2.]);
            value
        })
        .collect();
    let rgb: Vec<[f32; 3]> = (0..n).map(|i| [-20. - (i % 9) as f32, -10., -7.]).collect();
    let cache: Vec<[f32; 5]> = (0..3 * n)
        .map(|i| {
            [
                0.2 + (i % 5) as f32,
                0.3,
                0.1 + (i % 7) as f32,
                if i % 4 == 0 { 0. } else { 0.75 },
                1.,
            ]
        })
        .collect();
    write(&mut gpu, path, bytemuck::cast_slice(&paths))?;
    write(&mut gpu, colors[0], bytemuck::cast_slice(&rgb))?;
    write(&mut gpu, shifts, bytemuck::cast_slice(&cache))?;
    let bindings = binding(&mut gpu, None)?;
    let merge = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/restir-merge.spv")),
            c"restir_merge",
        ),
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.dispatch(merge, [2, 2, 1])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let actual = floats(&mut gpu, colors[1])?;
    let mut pairs = 0;
    for record in indices.as_chunks::<16>().0 {
        let ids: [u32; 4] = std::array::from_fn(|i| {
            u32::from_le_bytes(record[4 * i..4 * i + 4].try_into().unwrap())
        });
        let center = ids[0] as usize;
        let mut expected = rgb[center].map(f64::from);
        let mut mass = 1.;
        for (slot, donor) in ids[1..].iter().copied().enumerate() {
            if donor == u32::MAX {
                continue;
            }
            let donor = donor as usize;
            pairs += 1;
            let delta = difference(
                &paths[center],
                &paths[donor],
                &cache[3 * center + slot],
                &cache[3 * donor + slot],
            );
            for c in 0..3 {
                expected[c] += f64::from(rgb[donor][c]) + delta[c];
            }
            mass += 1.;
        }
        for c in 0..3 {
            let expected = expected[c] / mass;
            assert!(
                (f64::from(actual[3 * center + c]) - expected).abs()
                    <= 1e-5 * expected.abs().max(1.),
                "offset={center} actual={} expected={expected}",
                actual[3 * center + c]
            );
        }
    }
    assert!(pairs > 0, "测试须实际消费paired邻居");
    let view = gpu.view.as_ref().unwrap();
    let (accumulation, counts, prefix, hdr) =
        (view.accumulation, view.counts, view.prefix, view.output);
    write(
        &mut gpu,
        prefix,
        bytemuck::cast_slice(&vec![[0f32; 4]; 32 * 32]),
    )?;
    let mut clear = Commands::new(&gpu.context)?;
    clear.clear_buffer(accumulation)?;
    clear.clear_buffer(counts)?;
    clear.barrier();
    clear
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    gpu.restir
        .as_ref()
        .unwrap()
        .post(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let hdr = floats(&mut gpu, hdr)?;
    assert!(hdr.iter().all(|v| v.is_finite()));
    assert!(
        hdr.as_chunks::<4>()
            .0
            .iter()
            .any(|v| v[..3].iter().any(|v| *v < -0.1)),
        "线性post和aerial不能提前裁掉负CV校正"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
