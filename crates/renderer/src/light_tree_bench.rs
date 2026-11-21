//! 无窗口的 32 区块视距压力场景。全部实例复用准备期的两个模型 BLAS，姿态在设备端展开。
use super::*;
const RADIUS: i32 = 32;
fn prepare_lamps(gpu: &mut Gpu) -> Result<(), Error> {
    let mut catalog = b"PFM1".to_vec();
    catalog.extend_from_slice(&2u32.to_le_bytes());
    for faces in [1u32, 6] {
        catalog.extend_from_slice(&faces.to_le_bytes());
        for side in 0..faces {
            for (u, v) in [(-1f32, -1f32), (-1., 1.), (1., 1.), (1., -1.)] {
                let mut p = [0.; 3];
                let axis = (side / 2) as usize;
                let sign = if side % 2 == 0 { 1. } else { -1. };
                p[axis] = if faces == 1 { 0. } else { sign * 0.15 };
                p[(axis + 1) % 3] = sign * u * 0.15;
                p[(axis + 2) % 3] = v * 0.15;
                catalog.extend_from_slice(bytemuck::cast_slice(&[
                    p[0],
                    p[1],
                    p[2],
                    u * 0.5 + 0.5,
                    v * 0.5 + 0.5,
                ]));
            }
        }
    }
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
        &catalog,
        Default::default(),
    )?;
    Ok(())
}
fn publish_regions(gpu: &mut Gpu, layers: i32) -> Result<usize, Error> {
    let lamp_mesh = Mesh {
        quads: vec![plane(28., 32., 34., 1)],
    };
    // 相机位于 (0, 0) 区块。取半径 32 的整个正方形作为保守的可见区块包围盒。
    let axis = (-RADIUS).div_euclid(4)..=RADIUS.div_euclid(4);
    let mut regions = Vec::new();
    for x in axis.clone() {
        for y in -1..-1 + layers {
            for z in axis.clone() {
                regions.push(prime_scene::RenderRegion {
                    key: [x, y, z],
                    geometry_revision: 1,
                    content_revision: 1,
                    mesh: (&lamp_mesh).into(),
                });
            }
        }
    }
    let roots = regions.len();
    gpu.set_regions(prime_scene::RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &regions,
    })?;
    assert_eq!(
        gpu.context.light_regions(gpu.lighting_gpu.tree)?.len(),
        roots
    );
    Ok(roots)
}
fn submit_lamps(
    gpu: &mut Gpu,
    packet: &mut Vec<u8>,
    count: u32,
    faces: u32,
    layers: i32,
    tick: usize,
) -> Result<(), Error> {
    packet.clear();
    packet.extend_from_slice(b"PTF1");
    packet.extend_from_slice(bytemuck::cast_slice(&[0u32, count, 0]));
    packet.extend_from_slice(&[0; 24]);
    for k in 0..count {
        packet.extend_from_slice(&(u64::from(k) + 1).to_le_bytes());
        packet.extend_from_slice(bytemuck::cast_slice(&[
            0u32,
            4,
            u32::MAX,
            0,
            1,
            64,
            0,
            u32::MAX,
            u32::MAX,
        ]));
        packet.extend_from_slice(&1u64.to_le_bytes());
        packet.extend_from_slice(bytemuck::cast_slice(&[u32::from(faces == 6), u32::MAX]));
        let (s, c) = (tick as f32 * 0.07 + k as f32 * 0.12).sin_cos();
        let x = ((k * 593) % 1039) as f32 - 512. + s;
        let y = ((k * 89) % (layers as u32 * 64)) as f32 - 64.;
        let z = ((k * 331) % 1039) as f32 - 512. + s;
        packet.extend_from_slice(bytemuck::cast_slice(&[
            [c, 0., s, x],
            [0., 1., 0., y],
            [-s, 0., c, z],
        ]));
    }
    // SAFETY: 包内没有外部纹理。宿主在上一轮设备执行完成后才覆盖姿态。
    unsafe { gpu.set_world_features(packet) }
}

#[test]
#[ignore = "无窗口 Vulkan 时间戳；单独测世界灯树的 CPU 准备与 GPU 构建"]
fn hardware_light_tree_build_cost() -> Result<(), Box<dyn std::error::Error>> {
    const WARMUP: usize = 24;
    const SAMPLES: usize = 120;
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    prepare_lamps(gpu)?;
    let query = vulkan::native::TimestampQueries::new(&mut gpu.context, 4)?;
    gpu.test_roots.push(query.into());
    let raw = std::env::var_os("PRIME_LIGHT_BENCH_RAW").is_some();
    for layers in [1, 6] {
        let roots = publish_regions(gpu, layers)?;
        for (objects, faces_per_object) in [(0u32, 1u32), (10_000, 1), (10_000, 6)] {
            let n = objects * faces_per_object;
            let mut rows = Vec::with_capacity(SAMPLES);
            let mut packet = Vec::new();
            for tick in 0..(WARMUP + SAMPLES) {
                submit_lamps(gpu, &mut packet, objects, faces_per_object, layers, tick)?;
                let started = std::time::Instant::now();
                let rebound = gpu.features.lights.prepare(
                    &mut gpu.context,
                    gpu.features.surfaces,
                    gpu.lighting_gpu.tree,
                    gpu.anchor,
                )?;
                let cpu = started.elapsed().as_nanos() as u64;
                if rebound {
                    gpu.collect_resources()?;
                }
                let mut commands = Commands::new(&gpu.context)?;
                commands.reset_timestamps(query)?;
                gpu.features.record_poses(&mut commands)?;
                commands.barrier();
                commands.write_timestamp(query, 0)?;
                let started = std::time::Instant::now();
                gpu.features.lights.record_sort(&mut commands)?;
                commands.write_timestamp(query, 1)?;
                gpu.features.lights.record_power(&mut commands)?;
                commands.write_timestamp(query, 2)?;
                gpu.features.lights.record_branches(&mut commands)?;
                commands.barrier();
                commands.write_timestamp(query, 3)?;
                let encode = started.elapsed().as_nanos() as u64;
                commands
                    .finish(&mut gpu.context)?
                    .submit_and_wait(&mut gpu.context)?;
                gpu.features.accepted();
                let t = query
                    .read(&gpu.context)?
                    .ok_or("本次 GPU 建树时间戳未就绪")?;
                let ns = |a: usize, b: usize| query.elapsed_ns(&gpu.context, t[a], t[b]);
                let row = [cpu, ns(0, 1)?, ns(1, 2)?, ns(2, 3)?, ns(0, 3)?, encode];
                if tick >= WARMUP {
                    rows.push(row);
                }
            }
            if raw {
                for (i, r) in rows.iter().enumerate() {
                    eprintln!(
                        "tree_ns roots={roots} objects={objects} dynamic={n} sample={i} cpu={} collect_sort={} reduce={} branches={} gpu={} encode={}",
                        r[0], r[1], r[2], r[3], r[4], r[5]
                    );
                }
            }
            let stats: [[f64; 2]; 6] = std::array::from_fn(|k| {
                let mut values: Vec<_> = rows.iter().map(|r| r[k]).collect();
                values.sort_unstable();
                [
                    values[(SAMPLES - 1) / 2] as f64 / 1000.,
                    values[(SAMPLES * 95).div_ceil(100) - 1] as f64 / 1000.,
                ]
            });
            eprintln!(
                "tree_us view_chunks={RADIUS} roots={roots} objects={objects} dynamic={n} samples={SAMPLES} CPU_setup={:.3}/{:.3} collect_sort={:.3}/{:.3} reduce={:.3}/{:.3} branches={:.3}/{:.3} GPU={:.3}/{:.3} CPU_encode={:.3}/{:.3}",
                stats[0][0],
                stats[0][1],
                stats[1][0],
                stats[1][1],
                stats[2][0],
                stats[2][1],
                stats[3][0],
                stats[3][1],
                stats[4][0],
                stats[4][1],
                stats[5][0],
                stats[5][1]
            );
            if n > 0 {
                assert!(stats[4][0] > 0.);
                let tree = gpu.features.lights.tree();
                let leaves = (roots as u32 + n).next_power_of_two();
                let mut nodes = vec![[0u32; 4]; 7 * leaves as usize];
                let bytes = bytemuck::cast_slice_mut(&mut nodes);
                let copy = Buffer::new(&mut gpu.context, bytes.len() as u64, true)?;
                tree.copy_prefix_to(&mut gpu.context, &copy, bytes.len() as u64)?;
                copy.read(&mut gpu.context, 0, bytes)?;
                let [count, width, root, support] = nodes[0];
                assert_eq!(
                    (count, width, support),
                    (roots as u32 + n, leaves, roots as u32 + n)
                );
                let mut seen = vec![false; count as usize];
                let mut internal = vec![false; count as usize];
                let mut stack = vec![(root, 0, count)];
                while let Some((node, lo, hi)) = stack.pop() {
                    if node & 0x80000000 != 0 {
                        assert_eq!((node & 0x7fffffff, hi - lo), (lo, 1));
                        let source = nodes[(2 * width + lo) as usize][3];
                        let id = if source & 0x80000000 == 0 {
                            source
                        } else {
                            let region = source & 0x7fffffff;
                            assert_eq!(nodes[(3 * width + region) as usize][0], lo);
                            n + region
                        };
                        assert!(!std::mem::replace(&mut seen[id as usize], true));
                    } else {
                        assert!(lo < node && node < hi);
                        assert!(!std::mem::replace(&mut internal[node as usize], true));
                        let at = (4 * width + 3 * node) as usize;
                        let [a, b, sa, sb] = nodes[at + 2];
                        assert_eq!((sa, sb), (node - lo, hi - node));
                        assert!(
                            f32::from_bits(nodes[at][3]) > 0.
                                && f32::from_bits(nodes[at + 1][3]) > 0.
                        );
                        stack.extend([(a, lo, node), (b, node, hi)]);
                    }
                }
                assert!(seen.iter().all(|&v| v));

                // 模拟设备上整个实例批次熄灭，保持 CPU 源目录、数量与描述符不变。
                let mut cmd = Commands::new(&gpu.context)?;
                cmd.clear_buffer(gpu.features.surfaces)?;
                gpu.features.lights.record_sort(&mut cmd)?;
                gpu.features.lights.record_power(&mut cmd)?;
                gpu.features.lights.record_branches(&mut cmd)?;
                cmd.finish(&mut gpu.context)?
                    .submit_and_wait(&mut gpu.context)?;
                tree.copy_prefix_to(&mut gpu.context, &copy, 16)?;
                let mut header = [0u32; 4];
                copy.read(&mut gpu.context, 0, bytemuck::bytes_of_mut(&mut header))?;
                assert_eq!(header[0], 0, "GPU 熄灭全部灯面后应使用已有的静态世界目录");
                let mut cmd = Commands::new(&gpu.context)?;
                gpu.features.record_poses(&mut cmd)?;
                gpu.features.lights.record_sort(&mut cmd)?;
                gpu.features.lights.record_power(&mut cmd)?;
                gpu.features.lights.record_branches(&mut cmd)?;
                cmd.finish(&mut gpu.context)?
                    .submit_and_wait(&mut gpu.context)?;
                tree.copy_prefix_to(&mut gpu.context, &copy, 16)?;
                copy.read(&mut gpu.context, 0, bytemuck::bytes_of_mut(&mut header))?;
                assert_eq!(header[0], count, "在设备重放实例化后应恢复全部灯面");
            }
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan RT，32 区块视距与一万个 GPU 实例的原生 4K DLAA 整帧"]
fn hardware_light_tree_frame_cost() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(true, true)?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    prepare_lamps(gpu)?;
    let roots = publish_regions(gpu, 6)?;
    let output = Image::new(&mut gpu.context, [3840, 2160, 1], ImageFormat::Rgba8Unorm)?;
    let queries = [
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
    ];
    let timer = vulkan::native::TimestampQueries::new(&mut gpu.context, 2)?;
    gpu.features.light_timer = Some(timer);
    gpu.test_roots.push(timer.into());
    gpu.test_roots.push(output.into());
    gpu.test_roots
        .extend(queries.map(vulkan::native::Resource::from));
    let mut camera = camera();
    camera.up[1] = 2160. / 3840.;
    let opts = Options::new(true, streamline::Quality::Dlaa, 2, 8)?;
    let mut packet = Vec::new();
    for faces in [1, 6] {
        let mut gpu_ms = Vec::new();
        let mut tree_us = Vec::new();
        let mut wall_ms = Vec::new();
        for tick in 0..40 {
            let start = std::time::Instant::now();
            submit_lamps(gpu, &mut packet, 10_000, faces, 6, tick)?;
            let [_, ns] = host.render_profiled(
                gpu,
                &output,
                &camera,
                Environment::default(),
                opts,
                Some(queries),
            )?;
            if tick >= 8 {
                gpu_ms.push(ns as f64 * 1e-6);
                wall_ms.push(start.elapsed().as_secs_f64() * 1e3);
                let t = timer.read(&gpu.context)?.ok_or("帧内灯树时间戳未就绪")?;
                let tree_ns = timer.elapsed_ns(&gpu.context, t[0], t[1])?;
                tree_us.push(tree_ns as f64 / 1000.);
                if std::env::var_os("PRIME_LIGHT_BENCH_RAW").is_some() {
                    eprintln!(
                        "frame_ns dynamic={} sample={} gpu={ns} tree={tree_ns}",
                        faces * 10_000,
                        tick - 8
                    );
                }
            }
        }
        gpu_ms.sort_by(f64::total_cmp);
        wall_ms.sort_by(f64::total_cmp);
        tree_us.sort_by(f64::total_cmp);
        assert!(gpu_ms[15] > 0. && gpu.view.as_ref().unwrap().rr_enabled);
        eprintln!(
            "frame_ms native=3840x2160 DLAA spp=2 view_chunks={RADIUS} roots={roots} objects=10000 dynamic={} samples=32 GPU={:.3}/{:.3} wall={:.3}/{:.3} tree_us={:.3}/{:.3}",
            faces * 10_000,
            gpu_ms[15],
            gpu_ms[30],
            wall_ms[15],
            wall_ms[30],
            tree_us[15],
            tree_us[30]
        );
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
