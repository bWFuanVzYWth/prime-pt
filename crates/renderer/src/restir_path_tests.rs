//! 真实材料和多跳路径的重放恒等性；独立于CV颜色公式。
use super::*;
use prime_scene::{
    MaterialDefinition, MaterialId, RealtimeRenderer, RenderMode, RenderRegion, RenderView,
};
use simple_engine::vulkan::native::{RayTracingPipeline, Resource};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[test]
#[ignore = "真实RT；多跳路径在同一主表面重放必须保持储层integrand"]
fn hardware_pt_restir_replay_preserves_selected_integrand() -> Result {
    let material = std::env::var("RESTIR_REPLAY_MATERIAL").unwrap_or("sss".into());
    check_replayed_integrand(false, &material)
}

#[test]
#[ignore = "真实RT；跨像素路径映射再返回原像素须保持积分值和单位Jacobian"]
fn hardware_pt_restir_roundtrip_preserves_selected_integrand() -> Result {
    let material = std::env::var("RESTIR_REPLAY_MATERIAL").unwrap_or("sss".into());
    check_replayed_integrand(true, &material)
}

#[test]
#[ignore = "真实RT；吸收介质必须跨前缀重放与重连边界保留"]
fn hardware_pt_restir_absorbing_prefix_preserves_selected_integrand() -> Result {
    check_replayed_integrand(true, "solid")
}

#[test]
#[ignore = "真实RT；水中普通表面的前帧主射线须使用相机介质，保持路径积分值"]
fn hardware_pt_restir_previous_primary_preserves_camera_medium() -> Result {
    check_replayed_integrand(false, "camera_medium")
}

fn check_replayed_integrand(roundtrip: bool, material_case: &str) -> Result {
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let size: u32 = std::env::var("RESTIR_REPLAY_SIZE")
        .unwrap_or("128".into())
        .parse()?;
    let frames: u32 = std::env::var("RESTIR_REPLAY_FRAMES")
        .unwrap_or("64".into())
        .parse()?;
    let cv = std::env::var("RESTIR_REPLAY_CV").as_deref() == Ok("1");
    if size == 0 || frames < 2 {
        return Err("重放测试要求非零尺寸和至少两帧".into());
    }
    let output = Image::new(&mut gpu.context, [size, size, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut diffuse = MaterialDefinition::default();
    diffuse.base_roughness = [0.7, 0.6, 0.5, 0.8];
    diffuse.control[2] = if material_case == "all_cutout" {
        MaterialDefinition::CUTOUT
    } else {
        0
    };
    let mut leaf = diffuse;
    leaf.surface[2] = 0.6;
    match material_case {
        "sss" | "camera_medium" => {}
        "blend" => {
            leaf.base_roughness[3] = 0.12;
            leaf.surface[1] = 0.35;
        }
        "normal" => {
            leaf.control[2] |= MaterialDefinition::AUXILIARY;
            leaf.control[3] |= MaterialDefinition::AUTHORED_NORMAL;
        }
        "cutout" | "many_lights" | "cutout_lights" | "all_cutout" => {
            leaf.control[2] |= MaterialDefinition::CUTOUT
        }
        "many_lights_opaque" => {}
        "solid" | "thin" => {
            leaf.control[0] = if material_case == "solid" { 1 } else { 3 };
            leaf.base_roughness = [0.7, 0.8, 0.9, 0.25];
            leaf.surface = [1.5, 0., 0., 0.];
            leaf.extinction = [1.5, 0.05, 0.1, 0.2];
        }
        _ => return Err("未知的重放测试材料".into()),
    }
    let mut lamp = diffuse;
    lamp.control[1] = 3;
    if material_case == "cutout_lights" {
        lamp.control[2] |= MaterialDefinition::CUTOUT;
    }
    lamp.emission = [1., 0.8, 0.6, 20.];
    let definitions = [diffuse, leaf, lamp];
    let cutout = matches!(
        material_case,
        "cutout" | "many_lights" | "cutout_lights" | "all_cutout"
    );
    let atlas: Vec<u8> = (0..64)
        .flat_map(|i| {
            [
                255,
                255,
                255,
                if cutout && (i % 8 + i / 8) % 2 == 0 {
                    0
                } else {
                    255
                },
            ]
        })
        .collect();
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [8, 8],
        albedo: &atlas,
        materials: &definitions,
        mers: &[0, 0, 255, 0].repeat(64),
        auxiliary: &[190, 100, 0, 85].repeat(64),
        emission_alias_words: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 3],
    })?;
    let mut mesh = Mesh::default();
    for (positions, material) in [
        (
            [[-4., -4., 4.], [-4., 4., 4.], [4., 4., 4.], [4., -4., 4.]],
            0,
        ),
        (
            [
                [-4., -4., -2.],
                [4., -4., -2.],
                [4., 4., -2.],
                [-4., 4., -2.],
            ],
            0,
        ),
        (
            [
                [-4., -4., -2.],
                [-4., 4., -2.],
                [-4., 4., 4.],
                [-4., -4., 4.],
            ],
            0,
        ),
        (
            [[4., -4., 4.], [4., 4., 4.], [4., 4., -2.], [4., -4., -2.]],
            0,
        ),
        (
            [[-4., 4., 4.], [-4., 4., -2.], [4., 4., -2.], [4., 4., 4.]],
            0,
        ),
        (
            [
                [-4., -4., -2.],
                [-4., -4., 4.],
                [4., -4., 4.],
                [4., -4., -2.],
            ],
            0,
        ),
        (
            [[-1., -1., 2.], [-1., 1., 2.], [1., 1., 2.], [1., -1., 2.]],
            1,
        ),
        (
            [[0., -0.5, 1.], [0., 0.5, 1.], [0., 0.5, 2.], [0., -0.5, 2.]],
            2,
        ),
    ] {
        mesh.quads.push(Quad::with_material(
            positions,
            [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
            [1.; 3],
            MaterialId::new(material),
        ));
    }
    if material_case == "solid" {
        mesh.quads.push(Quad::with_material(
            [
                [1., -1., 2.1],
                [1., 1., 2.1],
                [-1., 1., 2.1],
                [-1., -1., 2.1],
            ],
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(1),
        ));
    }
    if matches!(
        material_case,
        "many_lights" | "many_lights_opaque" | "cutout_lights" | "all_cutout"
    ) {
        for x in 0..8 {
            for y in 0..8 {
                let x = x as f32 - 3.9;
                let y = y as f32 - 3.9;
                let r = 0.08;
                mesh.quads.push(Quad::with_material(
                    [
                        [x, y, 3.99],
                        [x, y + r, 3.99],
                        [x + r, y + r, 3.99],
                        [x + r, y, 3.99],
                    ],
                    [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
                    [1.; 3],
                    MaterialId::new(2),
                ));
            }
        }
    }
    gpu.set_rendering(RenderMode::Realtime, RealtimeRenderer::RestirPt)?;
    let fresh: f32 = std::env::var("RESTIR_REPLAY_FRESH")
        .unwrap_or("0".into())
        .parse()?;
    gpu.set_render_tuning(prime_scene::RenderTuning::new(20, fresh, false, cv)?)?;
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
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    if material_case == "camera_medium" {
        gpu.set_initial_medium([1.333, 0.2916, 0.04444, 0.010182])?;
    }
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 12)?;
    host.render_options(&mut gpu, &output, &camera(), environment, options)?;
    let readback = Buffer::new(
        &mut gpu.context,
        u64::from(size) * u64::from(size) * 5 * 16,
        true,
    )?;
    let (scene, _) = gpu.renderer_bindings(
        gpu.view.as_ref().unwrap(),
        &gpu.atlas,
        &gpu.scene,
        gpu.material_gpu,
        gpu.lighting_gpu,
    );
    let bindings = gpu.restir.as_ref().unwrap().test_bindings(
        &mut gpu.context,
        scene,
        gpu.restir_geometry.unwrap(),
        gpu.restir_lut,
        Some(readback),
    )?;
    let source = gpu
        .shader_source
        .with_file_name("restir_replay_probe.slang");
    let probe = gpu.compiler.compile(&shader_build::Recipe {
        source: source.to_str().ok_or("probe路径不是UTF-8")?,
        entry: "restir_replay_probe",
        stage: "raygeneration",
        output: "restir-replay-probe.spv",
        target: shader_build::Target::Spirv("spirv_1_4"),
        args: if material_case == "camera_medium" {
            &["-fp-mode", "precise", "-DRESTIR_PREVIOUS_PRIMARY_PROBE=1"]
        } else if roundtrip {
            &["-fp-mode", "precise", "-DRESTIR_ROUNDTRIP_PROBE=1"]
        } else {
            &["-fp-mode", "precise"]
        },
    })?;
    let generation = gpu.generation.as_ref().unwrap();
    let callables: Vec<_> = generation
        .restir_programs
        .iter()
        .enumerate()
        .skip(1)
        .take(6)
        .flat_map(|(role, set)| {
            set.artifacts().iter().map(move |artifact| RayShader {
                spirv: artifact.bytes(),
                entry: match role {
                    1 => c"restir_replay_flow",
                    2 => c"restir_evaluate_flow",
                    3 => c"restir_metadata_flow",
                    4 => c"restir_connection_flow",
                    5 => c"restir_emitter_flow",
                    _ => c"restir_pdf_flow",
                },
            })
        })
        .collect();
    let trace = super::super::programs(&[], false, materials::hit_features(&definitions));
    let pipeline = RayTracingPipeline::new(
        &mut gpu.context,
        bindings,
        [
            RayShader {
                spirv: probe.bytes(),
                entry: c"restir_replay_probe",
            },
            trace.miss,
            trace.closest_hit,
        ],
        trace.any_hit,
        &callables,
    )?;
    gpu.test_roots.extend([
        Resource::from(readback),
        Resource::from(bindings),
        Resource::from(pipeline),
    ]);
    let mut bytes = vec![0; readback.len() as usize];
    let mut mismatch = 0;
    let mut rejected = 0;
    let mut restir_sum = [0.; 3];
    for frame in 0..frames {
        if frame != 0 {
            host.render_options(&mut gpu, &output, &camera(), environment, options)?;
        }
        let mut commands = Commands::new(&gpu.context)?;
        commands.trace(pipeline, [size, size])?;
        commands.barrier();
        commands
            .finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        readback.read(&mut gpu.context, 0, &mut bytes)?;
        let mut peak = 0f32;
        let mut sum = [0.; 3];
        for (pixel, raw) in bytes.as_chunks::<80>().0.iter().enumerate() {
            let row: [f32; 20] = std::array::from_fn(|i| {
                f32::from_le_bytes(raw[4 * i..4 * i + 4].try_into().unwrap())
            });
            assert!(
                row[..8].iter().all(|v| v.is_finite()),
                "路径或重放包含非有限值"
            );
            if row[3] == 0. {
                continue;
            }
            peak = peak.max(row[0..3].iter().copied().fold(0., f32::max) * row[3]);
            for c in 0..3 {
                sum[c] += f64::from(row[c] * row[3]);
            }
            // 硬件命中与bary重建在近场存在浮点误差；支持域拒绝仍单独判失败。
            let bad = (row[7] - 1.).abs() > 3e-3
                || (0..3).any(|c| (row[c] - row[4 + c]).abs() > 5e-5 + 3e-3 * row[c].abs());
            if bad {
                rejected += usize::from(row[7] == 0.);
                if mismatch < 20 {
                    eprintln!(
                        "frame={frame} pixel={pixel} f={:?} replay={:?} flags={:08x} cached={:?} diagnostic={:?}",
                        &row[..4],
                        &row[4..8],
                        row[8].to_bits(),
                        &row[12..16],
                        &row[16..20]
                    );
                }
                mismatch += 1;
            }
        }
        let mean = sum.map(|v| v / f64::from(size * size));
        if frame >= frames / 2 {
            let output_mean = read_output_mean(&mut gpu, &readback, &mut bytes)?;
            for c in 0..3 {
                restir_sum[c] += output_mean[c];
            }
        }
        eprintln!(
            "frame={frame} peak={peak} mean={mean:?} cumulative_mismatch={mismatch} rejected={rejected}"
        );
    }
    gpu.set_rendering(RenderMode::Realtime, RealtimeRenderer::PathTrace)?;
    let reference_options = Options::new(false, streamline::Quality::Dlaa, 64, 12)?;
    let mut pt_sum = [0.; 3];
    for _ in 0..8 {
        host.render_options(&mut gpu, &output, &camera(), environment, reference_options)?;
        let mean = read_output_mean(&mut gpu, &readback, &mut bytes)?;
        for c in 0..3 {
            pt_sum[c] += mean[c];
        }
    }
    eprintln!(
        "reference cv={cv} pt_mean={:?} restir_mean={:?}",
        pt_sum.map(|v| v / 8.),
        restir_sum.map(|v| v / f64::from(frames - frames / 2))
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    assert_eq!(mismatch, 0, "恒等重放改变选中路径的integrand");
    Ok(())
}

fn read_output_mean(gpu: &mut Gpu, readback: &Buffer, bytes: &mut [u8]) -> Result<[f64; 3]> {
    let source = gpu.view.as_ref().unwrap().output;
    source.copy_prefix_to(&mut gpu.context, readback, source.len())?;
    let bytes = &mut bytes[..source.len() as usize];
    readback.read(&mut gpu.context, 0, bytes)?;
    let mut sum = [0.; 3];
    for raw in bytes.as_chunks::<16>().0 {
        for c in 0..3 {
            let value = f32::from_le_bytes(raw[4 * c..4 * c + 4].try_into().unwrap());
            assert!(value.is_finite(), "输出包含非有限值");
            sum[c] += f64::from(value);
        }
    }
    Ok(sum.map(|v| v / (bytes.len() / 16) as f64))
}

#[test]
#[ignore = "真实RT；共享MERS的玻璃必须在两种积分器中透出后方灯面"]
fn hardware_pt_mapped_glass_preserves_transmission() -> Result {
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = Image::new(&mut gpu.context, [64, 64, 1], ImageFormat::Rgba8Unorm)?;
    let readback = Buffer::new(&mut gpu.context, 64 * 64 * 16, true)?;
    gpu.test_roots
        .extend([Resource::from(output), Resource::from(readback)]);
    let mut bytes = vec![0; readback.len() as usize];
    for topology in [1, 3] {
        let mut glass = MaterialDefinition::default();
        glass.control = [topology, 1, MaterialDefinition::MERS, 0];
        glass.base_roughness = [1., 1., 1., 0.];
        glass.surface[0] = 1.5;
        glass.extinction = [1.5, 0., 0., 0.];
        let mut lamp = MaterialDefinition::default();
        lamp.base_roughness = [1., 1., 1., 1.];
        lamp.control[1] = 1;
        lamp.emission = [1., 1., 1., 1.];
        assert!(glass.valid() && lamp.valid());
        gpu.replace_resources(ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [1, 1],
            albedo: &[255; 4],
            materials: &[glass, lamp],
            mers: &[89, 0, 0, 0],
            auxiliary: &[128, 128, 0, 85],
            emission_alias_words: &[],
            light_areas: &[[u32::MAX, 0, 0, 0]; 2],
        })?;
        let mut mesh = Mesh::default();
        for (z, material, reverse) in [(1., 0, false), (2., 0, true), (4., 1, false)] {
            if topology == 3 && reverse {
                continue;
            }
            let mut positions = [[-8., -8., z], [-8., 8., z], [8., 8., z], [8., -8., z]];
            if reverse {
                positions.reverse();
            }
            mesh.quads.push(Quad::with_material(
                positions,
                [[0.5; 2]; 4],
                [1.; 3],
                MaterialId::new(material),
            ));
        }
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
            host.render_options(
                &mut gpu,
                &output,
                &camera(),
                Environment {
                    sun_direction: [0.; 3],
                    ..Default::default()
                },
                Options::new(false, streamline::Quality::Dlaa, 64, 12)?,
            )?;
            let mean = read_output_mean(&mut gpu, &readback, &mut bytes)?;
            eprintln!("glass topology={topology} renderer={renderer:?} mean={mean:?}");
            assert!(
                mean.iter().all(|&v| v > 0.7 && v < 1.1),
                "玻璃没有透出已知辐射率的灯面"
            );
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
