//! 真实资源世代与场景的无窗口行为验收；不替代Minecraft手工验收。
use super::*;
use prime_scene::{MaterialDefinition, MaterialId, RenderRegion, RenderView};

fn upload(
    gpu: &mut Gpu,
    definitions: &[MaterialDefinition],
    words: &[u32],
    areas: &[[u32; 4]],
) -> Result<(), Error> {
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [1, 1],
        albedo: &[255; 4],
        materials: definitions,
        mers: &[0, 255, 242, 0],
        auxiliary: &[128, 128, 0, 85],
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
    use light_sampling::emitter::{Coverage, EmissionProposal, EmitterAlias};
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
    assert_eq!(selectors[5], 5, "末灯移到rank1且保留data slot5");
    assert_eq!(selectors[9], 1, "新灯重用data slot1但位于rank5");
    assert_light_pdfs(&host, &mut gpu, &image, "swap_and_reuse")?;
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
