//! 真实资源世代与场景的无窗口行为验收；不替代Minecraft手工验收。
use super::*;
use prime_scene::{MaterialDefinition, MaterialId, RenderRegion, RenderView};

fn upload(
    gpu: &mut Gpu,
    definitions: &[MaterialDefinition],
    words: &[u32],
    areas: &[[u32; 4]],
) -> Result<(), Error> {
    upload_auxiliary(gpu, definitions, words, areas, &[128, 128, 0, 85])
}

fn upload_auxiliary(
    gpu: &mut Gpu,
    definitions: &[MaterialDefinition],
    words: &[u32],
    areas: &[[u32; 4]],
    auxiliary: &[u8],
) -> Result<(), Error> {
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [1, 1],
        albedo: &[255; 4],
        materials: definitions,
        mers: &[0, 255, 242, 0],
        auxiliary,
        emission_alias_words: words,
        light_areas: areas,
    })
}

fn publish(
    gpu: &mut Gpu,
    mesh: &Mesh,
    geometry: u64,
    content: u64,
    anchor: [f64; 3],
) -> Result<(), Error> {
    let regions = [RenderRegion {
        key: [0; 3],
        geometry_revision: geometry,
        content_revision: content,
        mesh: mesh.into(),
    }];
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        regions: &regions,
        anchor,
    })
}

#[test]
#[ignore = "真实Vulkan RT；白名单回退与默认材质发布相同的GPU参数和操作槽"]
fn hardware_pt_unsupported_recipe_publishes_default_material()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut gpu = host.gpu()?;
    gpu.experiment_dispatch = 8;
    let default = MaterialDefinition::default();
    let mut unsupported = default;
    unsupported.control[0] = 3;
    unsupported.surface[1] = 0.37;
    unsupported.emission = [4.; 4];
    upload(
        &mut gpu,
        &[default, unsupported],
        &[],
        &[[u32::MAX, 0, 0, 0]; 2],
    )?;
    let table = gpu.material_gpu.table;
    let bytes = (2 * size_of::<MaterialDefinition>()) as u64;
    let readback = Buffer::new(&mut gpu.context, bytes, true)?;
    table.copy_prefix_to(&mut gpu.context, &readback, bytes)?;
    let mut records = vec![0; bytes as usize];
    readback.read(&mut gpu.context, 0, &mut records)?;
    let (reference, fallback) = records.split_at(size_of::<MaterialDefinition>());
    assert_eq!(fallback, reference);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；资源准备后进入、退出世界仍可使用所有已准备渲染器"]
fn hardware_pt_resource_generation_survives_world_reset() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    upload(
        &mut gpu,
        &[MaterialDefinition::default()],
        &[],
        &[[u32::MAX, 0, 0, 0]],
    )?;
    for renderer in [
        prime_scene::RealtimeRenderer::PathTrace,
        prime_scene::RealtimeRenderer::RestirPt,
    ] {
        gpu.clear_world()?;
        gpu.set_rendering(prime_scene::RenderMode::Realtime, renderer)?;
        publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment::default(),
            Options::new(false, streamline::Quality::Dlaa, 1, 6)?,
        )?;
        let pixels = output.download(&mut gpu.context)?;
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[..3].iter().any(|&channel| channel != 0))
        );
    }
    gpu.clear_world()?;
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独立进程：实际SDK+EXT验证生产RR选择keyed Guide，原生程序不保留Guide SBT"]
fn hardware_pt_rr_keyed_program_preserves_opaque_and_glass_guides()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(true, true)?;
    let mut gpu = host.gpu()?;
    let extent = [512, 512];
    let output = Image::new(&mut gpu.context, [512, 512, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(output.into());
    let mut glass = MaterialDefinition::default();
    glass.control[0] = 1;
    glass.base_roughness[3] = 0.;
    glass.extinction = [1.5, 0., 0., 0.];
    let definitions = [MaterialDefinition::default(), glass];
    let mut reference = Vec::new();
    for mode in [0, 8] {
        gpu.experiment_dispatch = mode;
        upload(&mut gpu, &definitions, &[], &[[u32::MAX, 0, 0, 0]; 2])?;
        assert_eq!(gpu.generation.as_ref().unwrap().dispatch, mode);
        let generation = gpu.generation.as_ref().unwrap();
        assert_eq!(generation.guided_regular.is_some(), mode == 8);
        assert_eq!(generation.guided_reordered.is_some(), mode == 8);
        for with_glass in [false, true] {
            let mut mesh = Mesh::default();
            add_quad(&mut mesh, 3., [0.5; 2], [0.8, 0.5, 0.2], 10.);
            if with_glass {
                mesh.quads.push(Quad::with_material(
                    [
                        [-10., -10., 1.],
                        [-10., 10., 1.],
                        [10., 10., 1.],
                        [10., -10., 1.],
                    ],
                    [[0.5; 2]; 4],
                    [1.; 3],
                    MaterialId::new(1),
                ));
            }
            let revision = u64::from(with_glass) + 1;
            publish(&mut gpu, &mesh, revision, revision, [0.; 3])?;
            gpu.invalidate_history(GlobalReset::SceneDomainReplaced)?;
            let mut camera = camera();
            for frame in 0..2 {
                camera.origin[0] = frame as f32 * 0.125;
                host.render_rr(&mut gpu, &output, &camera, Environment::default())?;
                assert!(
                    gpu.rr_unavailable.is_none() && gpu.rr_previous.is_some(),
                    "SDK必须实际完成求值，不能以原生回退算通过"
                );
            }
            let view = gpu.view.as_ref().unwrap();
            assert!(view.rr_enabled);
            let mut parameters = vec![0; view.params.len() as usize];
            view.params.read(&mut gpu.context, 0, &mut parameters)?;
            let reconstruction_w = (16
                + 4
                + 4
                + gpu.display_drt.to_array().len()
                + 4
                + gpu.atmosphere.parameters().words().len()
                + 4
                + 3)
                * 4;
            assert_eq!(
                f32::from_le_bytes(parameters[reconstruction_w..reconstruction_w + 4].try_into()?),
                2.
            );
            let center = (extent[1] / 2 * extent[0] + extent[0] / 2) as usize;
            let depth = view.rr.images[1].download(&mut gpu.context)?;
            let depth = f32::from_le_bytes(depth[center * 4..center * 4 + 4].try_into()?);
            let normal = half_pixel(&view.rr.images[3].download(&mut gpu.context)?, extent, 4);
            let diffuse = half_pixel(&view.rr.images[4].download(&mut gpu.context)?, extent, 4);
            let specular = half_pixel(&view.rr.images[5].download(&mut gpu.context)?, extent, 4);
            let motion = half_pixel(&view.rr.images[2].download(&mut gpu.context)?, extent, 2);
            assert!(depth.is_finite() && depth > 1.);
            assert!((normal[2] + 1.).abs() < 0.001);
            assert!(diffuse[..3].iter().all(|&v| v > 0. && v < 1.));
            assert!(specular[..3].iter().all(|&v| v > 0. && v < 1.));
            assert!(motion[0] > 1. && motion[1].abs() < 0.01);
            if !with_glass {
                assert!((depth - 3.).abs() < 1e-4);
                assert!((motion[0] - 512. / 2. * 0.125 / 3.).abs() < 0.02);
            }
            let guides = std::iter::once(depth)
                .chain(normal)
                .chain(diffuse)
                .chain(specular)
                .chain(motion)
                .collect::<Vec<_>>();
            if mode == 0 {
                reference.push(guides);
            } else {
                for (actual, expected) in guides.iter().zip(&reference[usize::from(with_glass)]) {
                    assert!(
                        (actual - expected).abs() <= 0.01,
                        "glass={with_glass}，keyed={actual}，reference={expected}"
                    );
                }
            }
        }
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT：紧凑SBT的元数据/发光/逆PDF/法线操作须路由到本材质专用叶子"]
fn hardware_pt_material_operation_sbt_routes_all_material_keys()
-> Result<(), Box<dyn std::error::Error>> {
    use simple_engine::light_sampling::emitter::{Coverage, EmissionProposal, EmitterAlias};
    use simple_engine::vulkan::native::RayTracingPipeline;
    let host = Host::new_ser()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let mut definitions = vec![MaterialDefinition::default(); 11];
    definitions[1].surface[1] = 1.;
    definitions[1].base_roughness[3] = 0.;
    definitions[2].control[0] = 1;
    definitions[2].base_roughness[3] = 0.;
    definitions[3].control[0] = 1;
    definitions[3].extinction = [1.33, 0.03, 0.01, 0.005];
    definitions[3].base_roughness[3] = 0.5;
    definitions[4].control[0] = 3;
    definitions[4].base_roughness[3] = 0.3;
    definitions[5].control[2] = MaterialDefinition::MERS;
    definitions[6].control[2] = MaterialDefinition::AUXILIARY;
    definitions[6].control[3] = MaterialDefinition::AUTHORED_NORMAL;
    definitions[6].base_roughness[3] = 0.3;
    for (index, sides) in [(7, 1), (8, 2), (10, 3)] {
        definitions[index].control[1] = sides;
        definitions[index].emission = [0.8, 0.2, 0.6, 3.];
    }
    definitions[7].control[2] = MaterialDefinition::TEXTURED_EMISSION;
    definitions[9].control[2] =
        MaterialDefinition::MERS | MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS;
    definitions[9].overlay_bounds = [0., 0., 1., 1.];
    definitions[10].control[2] = MaterialDefinition::MERS
        | MaterialDefinition::AUXILIARY
        | MaterialDefinition::TEXTURED_EMISSION;
    definitions[10].control[3] = MaterialDefinition::AUTHORED_NORMAL | 2;
    assert!(definitions.iter().all(|definition| definition.valid()));
    let uv = [[0., 0.], [0., 1.], [1., 1.], [1., 0.]];
    let mut mesh = Mesh::default();
    for index in 0..definitions.len() {
        let x = (index % 4) as f32 * 2. - 4.;
        let y = (index / 4) as f32 * (8. / 3.) - 4.;
        let mut points = [
            [x, y, 4.],
            [x, y + 8. / 3., 4.],
            [x + 2., y + 8. / 3., 4.],
            [x + 2., y, 4.],
        ];
        if index == 8 {
            points.reverse();
        }
        mesh.quads.push(Quad::with_material(
            points,
            uv,
            [0.8, 0.7, 0.6],
            MaterialId::new(index as u32),
        ));
    }
    let proposal = EmissionProposal::new([2, 1], &[0., 1.], &[255, 255])?.unwrap();
    let alias = EmitterAlias::build(mesh.quads[10].positions(), uv, Coverage::Opaque, &proposal)?
        .ok_or("alias样本没有建立纹理提案")?;
    let mut words = vec![0];
    words.extend_from_slice(alias.words());
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [2, 1],
        albedo: &[255; 8],
        materials: &definitions,
        mers: &[64, 0, 153, 128, 192, 255, 204, 192],
        auxiliary: &[200, 128, 64, 85, 200, 128, 64, 85],
        emission_alias_words: &words,
        light_areas: &[[u32::MAX, 0, 0, 0]; 11],
    })?;
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        prime_scene::RealtimeRenderer::RestirPt,
    )?;
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    host.render_options(
        &mut gpu,
        &output,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Dlaa, 1, 1)?,
    )?;
    let readback = Buffer::new(&mut gpu.context, 32 * 32 * 23 * 16, true)?;
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
    let generation = gpu.generation.as_ref().unwrap();
    let guide_offset: usize = generation.restir_programs[1..7]
        .iter()
        .map(|set| set.artifacts().len())
        .sum();
    let define = format!("-DPRIMEPT_PROBE_GUIDE_OFFSET={guide_offset}");
    let source = gpu.shader_source.with_file_name("material_probe.slang");
    let probe = gpu.compiler.compile(&shader_build::Recipe {
        source: source.to_str().ok_or("probe路径不是UTF-8")?,
        entry: "material_route_probe",
        stage: "raygeneration",
        output: "material-route-probe.spv",
        target: shader_build::Target::Spirv("spirv_1_4"),
        args: &["-fp-mode", "precise", &define],
    })?;
    let callables: Vec<_> = generation
        .restir_programs
        .iter()
        .enumerate()
        .skip(1)
        .take(7)
        .flat_map(|(role, set)| {
            set.artifacts().iter().map(move |artifact| RayShader {
                spirv: artifact.bytes(),
                entry: match role {
                    1 => c"restir_replay_flow",
                    2 => c"restir_evaluate_flow",
                    3 => c"restir_metadata_flow",
                    4 => c"restir_connection_flow",
                    5 => c"restir_emitter_flow",
                    6 => c"restir_pdf_flow",
                    _ => c"guide_known",
                },
            })
        })
        .collect();
    let trace = super::super::programs(
        &[],
        false,
        materials::HitFeatures {
            coverage: 0,
            authored_normals: false,
        },
    );
    let pipeline = RayTracingPipeline::new(
        &mut gpu.context,
        bindings,
        [
            RayShader {
                spirv: probe.bytes(),
                entry: c"material_route_probe",
            },
            trace.miss,
            trace.closest_hit,
        ],
        trace.any_hit,
        &callables,
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.trace(pipeline, [32, 32])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let mut bytes = vec![0; readback.len() as usize];
    readback.read(&mut gpu.context, 0, &mut bytes)?;
    let values: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .collect();
    let mut seen = vec![0usize; definitions.len()];
    for (pixel, rows) in values.as_chunks::<{ 23 * 4 }>().0.iter().enumerate() {
        if rows[88] == 0. {
            continue;
        }
        let material = rows[89] as usize;
        assert!(
            material < seen.len(),
            "pixel={pixel}, invalid material={material}"
        );
        seen[material] += 1;
        for pair in 0..11 {
            for channel in 0..4 {
                let actual = rows[pair * 8 + channel];
                let expected = rows[pair * 8 + 4 + channel];
                assert!(
                    actual.is_finite()
                        && expected.is_finite()
                        && (actual - expected).abs() <= 2e-4 * expected.abs().max(1.),
                    "material={material}, pixel={pixel}, role_pair={pair}, channel={channel}: {actual} != {expected}"
                );
            }
        }
    }
    assert!(
        seen.iter().all(|&count| count > 0),
        "所有材质键都须实际命中: {seen:?}"
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；超过八种材质能力仍可执行ReSTIR的独立操作叶子"]
fn hardware_pt_restir_many_material_variants() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let definitions: Vec<_> = (0..12)
        .map(|index| {
            let mut material = MaterialDefinition {
                control: [[0, 1, 3][index % 3], 3, 0, 0],
                ..Default::default()
            };
            material.base_roughness[3] = if (index / 3) & 1 == 0 { 0. } else { 0.6 };
            if index >= 6 {
                material.emission = [0.25, 0.5, 0.75, 1.];
            }
            material
        })
        .collect();
    upload(&mut gpu, &definitions, &[], &[[u32::MAX, 0, 0, 0]; 12])?;
    assert!(
        gpu.generation
            .as_ref()
            .unwrap()
            .programs
            .plan()
            .keys()
            .len()
            > 8
    );
    let mut mesh = Mesh::default();
    for index in 0..12 {
        let x = (index % 4) as f32 - 2.;
        let y = (index / 4) as f32 - 1.5;
        mesh.quads.push(Quad::with_material(
            [
                [x, y, 4.],
                [x, y + 1., 4.],
                [x + 1., y + 1., 4.],
                [x + 1., y, 4.],
            ],
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(index),
        ));
    }
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        prime_scene::RealtimeRenderer::RestirPt,
    )?;
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
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
    assert!(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .all(|word| f32::from_le_bytes(*word).is_finite())
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan compute；以两个基址验证全局图元索引跨过24位"]
fn hardware_pt_region_primitive_index_keeps_full_quad_base()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    host.render_environment(&mut gpu, &image, &camera(), Environment::default())?;
    let view = gpu.view.as_ref().unwrap();
    let (mut bindings, _) = gpu.renderer_bindings(
        view,
        &gpu.atlas,
        &gpu.scene,
        gpu.material_gpu,
        gpu.lighting_gpu,
    );
    let bases = [1u32 << 23, (1u32 << 24) + 17];
    let base_buffer = Buffer::new(&mut gpu.context, 8, true)?;
    base_buffer.write(&mut gpu.context, 0, bytemuck::cast_slice(&bases))?;
    bindings[36] = Binding::StorageBuffer(base_buffer);
    let output = Buffer::new(&mut gpu.context, 16, true)?;
    bindings.push(Binding::StorageBuffer(output));
    let bindings = Bindings::new(&mut gpu.context, &bindings)?;
    let pipeline = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/region_primitive_probe.spv")),
            c"region_primitive_probe_main",
        ),
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.dispatch(pipeline, [1, 1, 1])?;
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let mut indices = [0u32; 4];
    output.read(&mut gpu.context, 0, bytemuck::cast_slice_mut(&mut indices))?;
    assert_eq!(
        indices,
        [
            bases[0] * 2,
            bases[0] * 2 + 1,
            bases[1] * 2,
            bases[1] * 2 + 3
        ]
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；区域实例顺序和属性槽分别变化时保留正确材质"]
fn hardware_pt_region_instance_order_is_independent_of_attribute_slot()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let materials = [[1., 0., 0., 10.], [0., 1., 0., 10.]].map(|emission| {
        let mut material = MaterialDefinition {
            emission,
            ..Default::default()
        };
        material.control[1] = 1;
        material
    });
    upload(&mut gpu, &materials, &[], &[[u32::MAX, 0, 0, 0]; 2])?;
    let meshes = [0, 1].map(|index| Mesh {
        quads: vec![Quad::with_material(
            [[-2., -2., 4.], [-2., 2., 4.], [2., 2., 4.], [2., -2., 4.]],
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(index),
        )],
    });
    let regions =
        [([-1, 0, 0], &meshes[1]), ([0; 3], &meshes[0])].map(|(key, mesh)| RenderRegion {
            key,
            geometry_revision: 1,
            content_revision: 1,
            mesh: mesh.into(),
        });
    let submit = |gpu: &mut Gpu, regions| {
        gpu.set_regions(RenderView {
            generation: gpu.resource_generation,
            regions,
            anchor: [0.; 3],
        })
    };
    submit(&mut gpu, &regions[1..])?;
    submit(&mut gpu, &regions)?;
    let entries = gpu.region_cache.entries();
    assert!(entries[0].range.start > entries[1].range.start);
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let display = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let pixel = center(&display);
    assert!(u16::from(pixel[0]) > u16::from(pixel[1]) + 20);

    submit(&mut gpu, &regions[..1])?;
    let left_camera = Camera {
        origin: [-64., 0., 0., 0.],
        ..camera()
    };
    let display = host.render_environment(&mut gpu, &image, &left_camera, environment)?;
    let pixel = center(&display);
    assert!(u16::from(pixel[1]) > u16::from(pixel[0]) + 20);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn scene() -> Mesh {
    Mesh {
        quads: vec![
            Quad::with_material(
                [[-4., -4., 4.], [-4., 4., 4.], [4., 4., 4.], [4., -4., 4.]],
                [[0.5; 2]; 4],
                [0.8; 3],
                MaterialId::new(0),
            ),
            // 朝向接收面的发光面；相机朝+Z，不直接看到位于左侧的灯。
            Quad::with_material(
                [
                    [-2., -0.5, 1.],
                    [-1., -0.5, 1.],
                    [-1., 0.5, 1.],
                    [-2., 0.5, 1.],
                ],
                [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
                [1.; 3],
                MaterialId::new(1),
            ),
        ],
    }
}

#[test]
#[ignore = "真实Vulkan RT/compute；太阳和局部灯各自采样且条件PDF与逆向密度一致"]
fn hardware_pt_restir_light_families_use_conditional_pdfs() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let output = image(&mut gpu)?;
    let receiver = MaterialDefinition::default();
    let mut lamp = receiver;
    lamp.emission = [1., 1., 1., 20.];
    lamp.control[1] = 3;
    upload(&mut gpu, &[receiver, lamp], &[], &[[u32::MAX, 0, 0, 0]; 2])?;
    gpu.set_rendering(
        prime_scene::RenderMode::Realtime,
        prime_scene::RealtimeRenderer::RestirPt,
    )?;
    for (case, (local, sky)) in [(true, true), (false, true), (true, false), (false, false)]
        .into_iter()
        .enumerate()
    {
        let mut mesh = scene();
        if !local {
            mesh.quads.truncate(1);
        }
        publish(&mut gpu, &mesh, case as u64 + 1, case as u64 + 1, [0.; 3])?;
        let environment = if sky {
            Environment::default()
        } else {
            Environment {
                sun_direction: [0.; 3],
                ..Default::default()
            }
        };
        host.render_environment(&mut gpu, &output, &camera(), environment)?;
        let readback = Buffer::new(&mut gpu.context, 32 * 32 * 32, true)?;
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
        let pipeline = ComputePipeline::new(
            &mut gpu.context,
            bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/restir_light_family_probe.spv")),
                c"restir_light_family_probe",
            ),
        )?;
        let mut commands = Commands::new(&gpu.context)?;
        commands.dispatch(pipeline, [2, 2, 1])?;
        commands.barrier();
        commands
            .finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        let mut bytes = vec![0; readback.len() as usize];
        readback.read(&mut gpu.context, 0, &mut bytes)?;
        let mut seen = [0; 2];
        for row in bytes.as_chunks::<32>().0 {
            let value = |i: usize| f32::from_le_bytes(row[4 * i..4 * i + 4].try_into().unwrap());
            for (family, enabled) in [local, sky].into_iter().enumerate() {
                let pdf = value(2 * family);
                let reverse = value(2 * family + 1);
                assert!(pdf.is_finite() && reverse.is_finite());
                if pdf > 0. {
                    assert!(enabled, "空灯族不能改投另一族");
                    assert!(
                        (pdf - reverse).abs() <= 2e-4 * pdf.max(1.),
                        "family={family}, pdf={pdf}, reverse={reverse}"
                    );
                    assert_eq!(value(4 + family).to_bits(), if family == 0 { 1 } else { 0 });
                    seen[family] += 1;
                } else if !enabled {
                    assert_eq!(pdf, 0.);
                }
            }
        }
        for (family, enabled) in [local, sky].into_iter().enumerate() {
            assert_eq!(seen[family] > 512, enabled, "case={case}, seen={seen:?}");
        }
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；纯tint只写变化quad且整批拒绝不部分写入"]
fn hardware_pt_sparse_tint_publication_keeps_geometry_and_writes_only_the_patch()
-> Result<(), Box<dyn std::error::Error>> {
    use prime_scene::{FragmentMask, MeshParts, PreparedParts, RenderChange, TintPatch};
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    upload(
        &mut gpu,
        &[MaterialDefinition::default(); 2],
        &[],
        &[[u32::MAX, 0, 0, 0]; 2],
    )?;
    let mut parts = MeshParts::default();
    parts.insert(0, scene())?;
    parts.insert(
        255,
        Mesh {
            quads: vec![scene().quads[0]; 200],
        },
    )?;
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: parts.view(),
        }],
    })?;
    let top = gpu.scene.top;
    let storage = gpu.scene.quads;
    let base = gpu.region_cache.entries()[0].range.start as u64 * 48;
    let mut before = vec![0; parts.len() * 48];
    storage.read(&mut gpu.context, base, &mut before)?;
    let candidate = PreparedParts::prepare(&parts, MeshParts::default(), FragmentMask::empty())?
        .with_tints(&parts, vec![TintPatch::new(255, 100, 1, 0xff12_3456)?])?;
    let region = RenderRegion {
        key: [0; 3],
        geometry_revision: 1,
        content_revision: 2,
        mesh: candidate.view(&parts),
    };
    let unsupported = [
        RenderChange::Upsert(region),
        RenderChange::Upsert(RenderRegion {
            key: [1, 0, 0],
            ..region
        }),
    ];
    assert!(!gpu.try_attribute_changes(gpu.resource_generation, [0.; 3], &unsupported)?);
    let mut after = vec![0; before.len()];
    storage.read(&mut gpu.context, base, &mut after)?;
    assert_eq!(after, before, "不支持的混合批不能提前写入前缀");
    assert!(gpu.try_attribute_changes(
        gpu.resource_generation,
        [0.; 3],
        &[RenderChange::Upsert(region)]
    )?);
    let timing = gpu.scene_timing().unwrap();
    assert_eq!(timing.attribute_upload_bytes, 48);
    assert_eq!(timing.light_upload_bytes, 0);
    assert_eq!(timing.acceleration_ns, 0);
    assert_eq!(timing.collect_ns, 0);
    assert_eq!(gpu.scene.top, top);
    assert_eq!(gpu.scene.quads, storage);
    storage.read(&mut gpu.context, base, &mut after)?;
    let offset = (2 + 100) * 48;
    assert_eq!(&after[..offset], &before[..offset]);
    assert_eq!(&after[offset + 48..], &before[offset + 48..]);
    let words: &[[[u32; 4]; 3]] = bytemuck::cast_slice(&after[offset..offset + 48]);
    assert_eq!(
        words[0][2][..3],
        [0x12, 0x34, 0x56].map(|v| (v as f32 / 255.).to_bits())
    );
    let _retired = candidate.finish(&mut parts);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；按变化范围写入，anchor与空发布不重写属性"]
fn hardware_pt_incremental_attributes_keep_storage_and_tlas()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let definitions = [MaterialDefinition::default(), MaterialDefinition::default()];
    upload(&mut gpu, &definitions, &[], &[[u32::MAX, 0, 0, 0]; 2])?;
    let mut mesh = scene();
    let unchanged = Mesh {
        quads: vec![mesh.quads[0]; 100],
    };
    let submit = |gpu: &mut Gpu, mesh: &Mesh, revision, anchor| {
        let regions = [
            RenderRegion {
                key: [0; 3],
                geometry_revision: 1,
                content_revision: revision,
                mesh: mesh.into(),
            },
            RenderRegion {
                key: [1, 0, 0],
                geometry_revision: 1,
                content_revision: 1,
                mesh: (&unchanged).into(),
            },
        ];
        gpu.set_regions(RenderView {
            generation: gpu.resource_generation,
            regions: &regions,
            anchor,
        })
    };
    submit(&mut gpu, &mesh, 1, [0.; 3])?;
    let storage = gpu.scene.quads;
    let top = gpu.scene.top;
    let blas = gpu.region_cache.entries()[0].blas;
    let initial_bytes = gpu.scene_timing().unwrap().attribute_upload_bytes;
    mesh.quads[0].set_tint([0.2, 0.8, 0.4]);
    submit(&mut gpu, &mesh, 2, [0.; 3])?;
    assert_eq!(gpu.scene.quads, storage);
    assert_eq!(gpu.scene.top, top);
    assert_eq!(gpu.region_cache.entries()[0].blas, blas);
    let timing = gpu.scene_timing().unwrap();
    assert_eq!(timing.attribute_upload_bytes, 96);
    assert!(timing.attribute_upload_bytes < initial_bytes);
    assert_eq!(timing.tlas_ns, 0);
    assert_eq!(timing.dirty_regions, 0);
    submit(&mut gpu, &mesh, 2, [0.; 3])?;
    let timing = gpu.scene_timing().unwrap();
    assert!(!timing.changed);
    assert_eq!(timing.attribute_upload_bytes, 0);
    assert_eq!(timing.collect_ns, 0);
    submit(&mut gpu, &mesh, 2, [64., 0., 0.])?;
    let timing = gpu.scene_timing().unwrap();
    assert_eq!(timing.attribute_upload_bytes, 0);
    assert_eq!(timing.dirty_regions, 0);
    assert_ne!(gpu.scene.top, top);
    assert_eq!(gpu.scene.quads, storage);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；扩容保留旧属性槽，只写入新增region"]
fn hardware_pt_attribute_growth_keeps_existing_gpu_bytes() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    upload(
        &mut gpu,
        &[MaterialDefinition::default(); 2],
        &[],
        &[[u32::MAX, 0, 0, 0]; 2],
    )?;
    let mesh = scene();
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    let old = gpu.scene.quads;
    let offset = u64::from(gpu.region_cache.entries()[0].range.start) * 48;
    let mut before = [0u8; 96];
    old.read(&mut gpu.context, offset, &mut before)?;
    let extra = Mesh {
        quads: vec![mesh.quads[0]; 4096],
    };
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[
            RenderRegion {
                key: [0; 3],
                geometry_revision: 1,
                content_revision: 1,
                mesh: (&mesh).into(),
            },
            RenderRegion {
                key: [1, 0, 0],
                geometry_revision: 1,
                content_revision: 1,
                mesh: (&extra).into(),
            },
        ],
    })?;
    assert_ne!(gpu.scene.quads, old);
    let mut after = [0u8; 96];
    gpu.scene.quads.read(&mut gpu.context, offset, &mut after)?;
    assert_eq!(after, before);
    assert_eq!(
        gpu.scene_timing().unwrap().attribute_upload_bytes,
        4096 * 48 + 8
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；看不到灯的接收面通过NEE变亮，单/双面发光遵守同一PDF"]
fn hardware_pt_emissive_quad_lights_the_receiver_from_the_correct_side()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let receiver = MaterialDefinition::default();
    let mut lamp = receiver;
    lamp.emission = [1., 1., 1., 20.];
    let mesh = scene();
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let mut totals = Vec::new();
    for sides in [1, 2, 3] {
        lamp.control[1] = sides;
        upload(&mut gpu, &[receiver, lamp], &[], &[[u32::MAX, 0, 0, 0]; 2])?;
        publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
        let display = host.render_environment(&mut gpu, &image, &camera(), environment)?;
        // 最右侧可见接收面与发光面投影不重叠；没有天空或其它发光源。
        totals.push(
            (0..32)
                .flat_map(|y| (24..32).map(move |x| (y * 32 + x) * 4))
                .map(|at| {
                    display[at..at + 3]
                        .iter()
                        .map(|&v| u64::from(v))
                        .sum::<u64>()
                })
                .sum::<u64>(),
        );
    }
    assert!(
        totals[0] > 0,
        "正面光源应照亮未直接可见的接收面：{totals:?}"
    );
    assert_eq!(totals[1], 0, "背侧发光不应从反面照亮接收面：{totals:?}");
    assert!(totals[2] > 0, "双面发光保持正面支持：{totals:?}");
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan RT；方块 EV 同比例改变命中发光与受光面的 NEE"]
fn hardware_pt_block_ev_doubles_visible_and_sampled_emission()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let lamp = MaterialDefinition {
        emission: [1., 1., 1., 20.],
        ..Default::default()
    };
    upload(
        &mut gpu,
        &[MaterialDefinition::default(), lamp],
        &[],
        &[[u32::MAX, 0, 0, 0]; 2],
    )?;
    let mut mesh = scene();
    mesh.quads.push(Quad::with_material(
        [
            [-0.4, -0.4, 1.],
            [-0.4, 0.4, 1.],
            [0.4, 0.4, 1.],
            [0.4, -0.4, 1.],
        ],
        [[0.5; 2]; 4],
        [1.; 3],
        MaterialId::new(1),
    ));
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    exposure_view(&mut gpu)?;
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Environment::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 4, 4)?;
    host.render_options(&mut gpu, &image, &camera(), environment, options)?;
    let baseline = accumulated_rgb(&mut gpu)?;
    gpu.set_lighting_options(0., 1.)?;
    host.render_options(&mut gpu, &image, &camera(), environment, options)?;
    let brighter = accumulated_rgb(&mut gpu)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 4);
    let receiver = |pixels: &[[f32; 3]]| -> f32 {
        pixels
            .as_chunks::<32>()
            .0
            .iter()
            .flat_map(|row| &row[24..])
            .map(|p| p[0])
            .sum()
    };
    for (before, after) in [
        (baseline[16 * 32 + 16][0], brighter[16 * 32 + 16][0]),
        (receiver(&baseline), receiver(&brighter)),
    ] {
        assert!(
            before > 0.0 && (after - 2. * before).abs() < 1e-5 * before,
            "方块发光 EV {before} -> {after}"
        );
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；错误静态alias与错误region世代保留已发布资源"]
fn hardware_pt_rejected_resource_domain_preserves_the_published_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    upload(
        &mut gpu,
        &[MaterialDefinition::default(); 2],
        &[],
        &[[u32::MAX, 0, 0, 0]; 2],
    )?;
    let mesh = scene();
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    let previous = (
        gpu.scene.top,
        gpu.scene.quads,
        gpu.material_gpu.table,
        gpu.resource_generation,
    );
    let mut invalid = MaterialDefinition::default();
    invalid.control[3] = 2;
    assert!(upload(&mut gpu, &[invalid], &[0], &[[u32::MAX, 0, 0, 0]]).is_err());
    assert_eq!(
        (
            gpu.scene.top,
            gpu.scene.quads,
            gpu.material_gpu.table,
            gpu.resource_generation
        ),
        previous
    );
    let regions = [RenderRegion {
        key: [0; 3],
        geometry_revision: 2,
        content_revision: 2,
        mesh: (&mesh).into(),
    }];
    assert!(
        gpu.set_regions(RenderView {
            generation: 2,
            regions: &regions,
            anchor: [0.; 3]
        })
        .is_err()
    );
    assert_eq!(
        (
            gpu.scene.top,
            gpu.scene.quads,
            gpu.material_gpu.table,
            gpu.resource_generation
        ),
        previous
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；资源期texel alias与过程非共面quad的正逆PDF逐样本一致"]
fn hardware_pt_static_emission_alias_and_dynamic_area_match_reverse_pdf()
-> Result<(), Box<dyn std::error::Error>> {
    use simple_engine::light_sampling::emitter::{Coverage, EmissionProposal, EmitterAlias};
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let receiver = MaterialDefinition::default();
    let mut lamp = receiver;
    lamp.emission = [1., 1., 1., 20.];
    lamp.control[2] |= MaterialDefinition::MERS | MaterialDefinition::TEXTURED_EMISSION;
    let mut mesh = scene();
    let uv = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]];
    let proposal = EmissionProposal::new([2, 1], &[0., 1.], &[255, 255])?.unwrap();
    let table =
        EmitterAlias::build(mesh.quads[1].positions(), uv, Coverage::Opaque, &proposal)?.unwrap();
    let mut words = vec![0];
    words.extend_from_slice(table.words());
    for static_alias in [true, false] {
        lamp.control[3] = if static_alias { 2 } else { 0 };
        if !static_alias {
            let mut points = mesh.quads[1].positions();
            points[3][2] += 0.2;
            mesh.quads[1] = Quad::with_material(points, uv, [1.; 3], MaterialId::new(1));
        }
        gpu.replace_resources(ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [2, 1],
            albedo: &[255; 8],
            materials: &[receiver, lamp],
            mers: if static_alias {
                &[0, 0, 242, 0, 0, 255, 242, 0]
            } else {
                &[0, 255, 242, 0, 0, 255, 242, 0]
            },
            auxiliary: &[128, 128, 0, 85, 128, 128, 0, 85],
            emission_alias_words: if static_alias { &words } else { &[] },
            light_areas: &[[u32::MAX, 0, 0, 0]; 2],
        })?;
        publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
        assert_light_pdfs(
            &host,
            &mut gpu,
            &image,
            if static_alias { "static" } else { "dynamic" },
        )?;
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn assert_light_pdfs(
    host: &Host,
    gpu: &mut Gpu,
    image: &Image,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    host.render_environment(
        gpu,
        image,
        &camera(),
        Environment {
            sun_direction: [0.; 3],
            ..Default::default()
        },
    )?;
    let view = gpu.view.as_ref().unwrap();
    let (mut bindings, _) = gpu.renderer_bindings(
        view,
        &gpu.atlas,
        &gpu.scene,
        gpu.material_gpu,
        gpu.lighting_gpu,
    );
    let output = Buffer::new(&mut gpu.context, 32 * 16, true)?;
    bindings.push(Binding::StorageBuffer(output));
    let bindings = Bindings::new(&mut gpu.context, &bindings)?;
    let pipeline = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/light_probe.spv")),
            c"light_probe_main",
        ),
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.dispatch(pipeline, [1, 1, 1])?;
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let mut bytes = vec![0; 32 * 16];
    output.read(&mut gpu.context, 0, &mut bytes)?;
    for record in bytes.as_chunks::<16>().0 {
        let value: [f32; 4] = std::array::from_fn(|i| {
            f32::from_le_bytes(record[i * 4..i * 4 + 4].try_into().unwrap())
        });
        assert!(
            value.iter().all(|v| v.is_finite()) && value[3] == 1.,
            "mode={label}, probe={value:?}"
        );
        assert!(
            (value[0] - value[1]).abs() / (1. + value[0].max(value[1])) < 1e-5,
            "mode={label}, forward/reverse={value:?}"
        );
    }
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；稳定灯数据槽与紧凑选择rank分离后正逆PDF保持一致"]
fn hardware_pt_light_slot_reuse_keeps_forward_and_reverse_pdf_equal()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let dark = MaterialDefinition::default();
    let lamp = MaterialDefinition {
        emission: [1., 1., 1., 20.],
        ..dark
    };
    upload(&mut gpu, &[dark, lamp], &[], &[[u32::MAX, 0, 0, 0]; 2])?;
    let quad = scene().quads[1];
    let mut mesh = Mesh {
        quads: vec![quad; 6],
    };
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    let lighting = gpu.lighting_gpu;
    mesh.quads[1] =
        Quad::with_material(quad.positions(), quad.uv(), quad.tint(), MaterialId::new(0));
    publish(&mut gpu, &mesh, 1, 2, [0.; 3])?;
    assert_eq!(gpu.lighting_gpu.roots(), lighting.roots());
    mesh.quads.push(quad);
    publish(&mut gpu, &mesh, 2, 3, [0.; 3])?;
    assert_eq!(gpu.lighting_gpu.roots(), lighting.roots());
    let mut bytes = vec![0; lighting.alias.len() as usize];
    lighting.alias.read(&mut gpu.context, 0, &mut bytes)?;
    let selectors: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    assert_eq!(selectors[0], 6);
    assert_eq!(selectors[9], 5, "末灯移到rank1且保留data slot5");
    assert_eq!(selectors[13], 1, "新灯重用data slot1但位于rank5");
    assert_light_pdfs(&host, &mut gpu, &image, "swap_and_reuse")?;
    mesh.quads[2] = Quad::with_material(
        quad.positions()
            .map(|point| [point[0], point[1], point[2] + 0.25]),
        quad.uv(),
        quad.tint(),
        quad.material_id(),
    );
    publish(&mut gpu, &mesh, 3, 4, [0.; 3])?;
    assert_eq!(gpu.lighting_gpu.roots(), lighting.roots());
    assert_light_pdfs(&host, &mut gpu, &image, "one_emitter_moved")?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；共面异材质双面保留各自属性与资源期callable"]
fn hardware_pt_bilateral_distinct_materials_select_the_visible_front()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut red = MaterialDefinition {
        emission: [1., 0., 0., 10.],
        ..Default::default()
    };
    red.control[1] = 1;
    red.control[2] |= MaterialDefinition::FRONT_ONLY;
    let mut green = red;
    green.emission = [0., 1., 0., 10.];
    green.base_roughness[3] = 0.;
    green.surface[1] = 1.;
    upload(&mut gpu, &[red, green], &[], &[[u32::MAX, 0, 0, 0]; 2])?;
    let points = [[-2., -2., 4.], [-2., 2., 4.], [2., 2., 4.], [2., -2., 4.]];
    let mesh = Mesh {
        quads: vec![
            Quad::with_material(points, [[0.5; 2]; 4], [1.; 3], MaterialId::new(0)),
            Quad::with_material(
                [points[3], points[2], points[1], points[0]],
                [[0.5; 2]; 4],
                [1.; 3],
                MaterialId::new(1),
            ),
        ],
    };
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let front = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let back = Camera {
        origin: [0., 0., 8., 0.],
        forward: [0., 0., -1., 0.],
        right: [-1., 0., 0., 0.],
        ..camera()
    };
    let back = host.render_environment(&mut gpu, &image, &back, environment)?;
    assert!(
        u16::from(center(&front)[0]) > u16::from(center(&front)[1]) + 20,
        "正面应为红材质：{:?}",
        center(&front)
    );
    assert!(
        u16::from(center(&back)[1]) > u16::from(center(&back)[0]) + 20,
        "背面应为绿材质：{:?}",
        center(&back)
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；仅更换材质即恢复覆盖测试，区域内不透明几何仍可见"]
fn hardware_pt_region_material_change_restores_cutout_coverage()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut red = MaterialDefinition {
        emission: [1., 0., 0., 10.],
        ..Default::default()
    };
    red.control[2] = 0;
    let mut green = red;
    green.emission = [0., 1., 0., 10.];
    let mut cutout = red;
    cutout.control[2] = MaterialDefinition::CUTOUT;
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [1, 1],
        albedo: &[255, 255, 255, 0],
        materials: &[red, green, cutout],
        mers: &[0, 0, 242, 0],
        auxiliary: &[128, 128, 0, 85],
        emission_alias_words: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 3],
    })?;
    let quad = |z, material| {
        Quad::with_material(
            [[-2., -2., z], [-2., 2., z], [2., 2., z], [2., -2., z]],
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(material),
        )
    };
    let mut mesh = Mesh {
        quads: vec![quad(4., 0), quad(5., 1)],
    };
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    assert!(gpu.region_cache.entries()[0].opaque);
    let before = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    mesh.quads[0] = quad(4., 2);
    publish(&mut gpu, &mesh, 1, 2, [0.; 3])?;
    assert!(!gpu.region_cache.entries()[0].opaque);
    let after = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert!(u16::from(center(&before)[0]) > u16::from(center(&before)[1]) + 20);
    assert!(u16::from(center(&after)[1]) > u16::from(center(&after)[0]) + 20);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；同一不透明quad的镂空覆盖只染覆盖层"]
fn hardware_pt_grass_overlay_selects_tinted_grass_and_untinted_dirt()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let material = MaterialDefinition {
        uv_bounds: [0., 0., 0.5, 1.],
        overlay_bounds: [0.5, 0., 1., 1.],
        control: [
            0,
            1,
            MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS,
            0,
        ],
        ..Default::default()
    };
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [4, 1],
        albedo: &[
            255, 0, 0, 255, 255, 0, 0, 255, 255, 255, 255, 0, 255, 255, 255, 255,
        ],
        materials: &[material],
        mers: &[0, 0, 242, 0].repeat(4),
        auxiliary: &[128, 128, 0, 85].repeat(4),
        emission_alias_words: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]],
    })?;
    let mesh = Mesh {
        quads: vec![Quad::with_material(
            [[-4., -4., 4.], [-4., 4., 4.], [4., 4., 4.], [4., -4., 4.]],
            [[0., 1.], [0., 0.], [0.5, 0.], [0.5, 1.]],
            [0., 1., 0.],
            MaterialId::new(0),
        )],
    };
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    assert!(gpu.region_cache.entries()[0].opaque);
    let display = host.render_environment(
        &mut gpu,
        &image,
        &camera(),
        Environment {
            sun_direction: [0., 0.6, -0.8],
            ..Default::default()
        },
    )?;
    let pixel = |x: usize| &display[(16 * 32 + x) * 4..][..3];
    let dirt = pixel(8);
    let grass = pixel(24);
    assert!(
        u16::from(dirt[0]) > u16::from(dirt[1]) + 16,
        "底层保留红色 {dirt:?}"
    );
    assert!(
        u16::from(grass[1]) > u16::from(grass[0]) + 16,
        "覆盖层使用绿色tint {grass:?}"
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实EXT SER/Vulkan RT；HG保留法线贴图在几何背面产生的连续光照支持"]
fn hardware_pt_hit_group_preserves_authored_normal_light_support()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let uv = [[0., 0.], [0., 1.], [1., 1.], [1., 0.]];
    let mesh = Mesh {
        quads: vec![
            // Ng=-Z朝向相机；UV切线为+X，AUX把Ns倾斜至约(.8,0,-.6)。
            Quad::with_material(
                [[-4., -4., 4.], [-4., 4., 4.], [4., 4., 4.], [4., -4., 4.]],
                uv,
                [1.; 3],
                MaterialId::new(0),
            ),
            // 灯位于Ng背侧，中心到接收面方向约(.95,0,.32)，但处于Ns前侧。
            Quad::with_material(
                [
                    [3., -0.5, 4.7],
                    [3., -0.5, 5.3],
                    [3., 0.5, 5.3],
                    [3., 0.5, 4.7],
                ],
                uv,
                [1.; 3],
                MaterialId::new(1),
            ),
        ],
    };
    let mut reference = Vec::<[f32; 4]>::new();
    for (dispatch, authored) in [(0, true), (8, true), (8, false)] {
        let mut gpu = host.gpu()?;
        gpu.experiment_dispatch = dispatch;
        let output = image(&mut gpu)?;
        let mut receiver = MaterialDefinition {
            base_roughness: [0.8, 0.8, 0.8, 1.],
            ..MaterialDefinition::default()
        };
        receiver.control[2] |= MaterialDefinition::AUXILIARY;
        if authored {
            receiver.control[3] |= MaterialDefinition::AUTHORED_NORMAL;
        }
        let mut lamp = MaterialDefinition {
            emission: [1., 1., 1., 20.],
            ..MaterialDefinition::default()
        };
        lamp.control[1] = 3;
        // 所有模式只在这个完整资源世代边界准备程序；绘制阶段不编译shader。
        upload_auxiliary(
            &mut gpu,
            &[receiver, lamp],
            &[],
            &[[u32::MAX, 0, 0, 0]; 2],
            &[230, 128, 0, 85],
        )?;
        publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
        gpu.set_ser(true)?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(20, 0., true, false)?)?;
        gpu.test_restart_sample_sequence(0)?;
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment {
                sun_direction: [0.; 3],
                ..Default::default()
            },
            Options::new(false, streamline::Quality::Dlaa, 16, 1)?,
        )?;
        let hdr = gpu.view.as_ref().unwrap().output;
        let readback = Buffer::new(&mut gpu.context, hdr.len(), true)?;
        hdr.copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
        let mut bytes = vec![0; hdr.len() as usize];
        readback.read(&mut gpu.context, 0, &mut bytes)?;
        let pixels: Vec<[f32; 4]> = bytes
            .as_chunks::<16>()
            .0
            .iter()
            .map(|pixel| {
                std::array::from_fn(|i| {
                    f32::from_le_bytes(pixel[4 * i..4 * i + 4].try_into().unwrap())
                })
            })
            .collect();
        assert!(pixels.iter().flatten().all(|value| value.is_finite()));
        let center_mean = (12..20)
            .flat_map(|y| (12..20).map(move |x| y * 32 + x))
            .map(|pixel| f64::from(pixels[pixel][0]))
            .sum::<f64>()
            / 64.;
        if authored {
            assert!(
                center_mean > 0.01,
                "mode={dispatch}, tilted normal must receive backside light: {center_mean}"
            );
            if dispatch == 0 {
                reference = pixels;
            } else {
                for (pixel, (actual, expected)) in pixels.iter().zip(&reference).enumerate() {
                    for channel in 0..4 {
                        assert!(
                            (actual[channel] - expected[channel]).abs()
                                <= 5e-5 * expected[channel].abs().max(1.),
                            "mode={dispatch}, pixel={pixel}, channel={channel}: {} != {}",
                            actual[channel],
                            expected[channel]
                        );
                    }
                }
            }
        } else {
            assert!(
                center_mean <= 1e-8,
                "flat normal must reject the same backside light: {center_mean}"
            );
        }
        drop(gpu);
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
