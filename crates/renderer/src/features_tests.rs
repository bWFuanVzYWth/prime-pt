//! 真实资源世代与场景的无窗口行为验收；不替代Minecraft手工验收。
use super::*;
use prime_scene::{MaterialDefinition, MaterialId, RenderRegion, RenderView};

fn publish_stream(
    gpu: &mut Gpu,
    mesh: &Mesh,
    geometry: u64,
    content: u64,
    anchor: [f64; 3],
) -> Result<(), Error> {
    let start = std::time::Instant::now();
    loop {
        if gpu.stream_regions(RenderView {
            generation: gpu.resource_generation,
            anchor,
            regions: &[RenderRegion {
                key: [0; 3],
                geometry_revision: geometry,
                content_revision: content,
                mesh: mesh.into(),
            }],
        })? {
            return Ok(());
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
        std::thread::yield_now();
    }
}

#[test]
#[ignore = "真实 Vulkan；大批纯 tint 在后台准备，保持 BLAS 和未变化属性"]
fn streamed_large_tint_only_touches_the_requested_faces() -> Result<(), Box<dyn std::error::Error>>
{
    use prime_scene::{FragmentMask, MeshParts, PreparedParts, RenderChange, TintPatch};
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    let mut base = MeshParts::default();
    base.insert(
        11,
        Mesh {
            quads: vec![scene().quads[0]; 2048],
        },
    )?;
    let region = RenderRegion {
        key: [0; 3],
        geometry_revision: 1,
        content_revision: 1,
        mesh: base.view(),
    };
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[region],
    })?;
    let top = gpu.scene.top;
    let patch = PreparedParts::prepare(&base, MeshParts::default(), FragmentMask::empty())?
        .with_tints(&base, vec![TintPatch::new(11, 137, 1000, 0xff80_4020)?])?;
    let updated = RenderRegion {
        content_revision: 2,
        mesh: patch.view(&base),
        ..region
    };
    assert!(!gpu.try_attribute_changes(
        gpu.resource_generation,
        [0.; 3],
        &[RenderChange::Upsert(updated)]
    )?);
    let start = std::time::Instant::now();
    let mut source = 0;
    loop {
        let ready = gpu.stream_regions(RenderView {
            generation: gpu.resource_generation,
            anchor: [0.; 3],
            regions: &[updated],
        })?;
        let t = gpu.scene_timing().unwrap();
        source += t.cpu_source_bytes;
        assert_eq!(t.build_quads, 0);
        if ready {
            break;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(15));
        std::thread::yield_now();
    }
    assert_eq!(source, 1000 * 48);
    assert_eq!(gpu.scene.top, top);
    host.render_environment(&mut gpu, &output, &camera(), Environment::default())?;
    gpu.complete_previous()?;
    let base = u64::from(gpu.region_cache.entries()[0].range.start);
    let mut rows = [[0u32; 12]; 2];
    gpu.scene.quads.read(
        &mut gpu.context,
        (base + 136) * 48,
        bytemuck::cast_slice_mut(&mut rows),
    )?;
    assert_eq!(
        &rows[1][8..11],
        &simple_engine::color::Srgb8Rgba::new(128, 64, 32, 255)
            .to_linear_srgb()
            .to_array()
            .map(f32::to_bits)[..3]
    );
    assert_ne!(rows[0][8], rows[1][8]);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；压紧包一次传完时仍将独立 BLAS 分散到不同帧"]
fn streamed_build_budget_splits_regions_and_keeps_the_old_top()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let top = gpu.scene.top;
    let mesh = Mesh {
        quads: vec![scene().quads[0]; 80_000],
    };
    let regions = [0, 1].map(|x| RenderRegion {
        key: [x, 0, 0],
        geometry_revision: 1,
        content_revision: 1,
        mesh: (&mesh).into(),
    });
    let mut batches = Vec::new();
    let start = std::time::Instant::now();
    loop {
        let done = gpu.stream_regions(RenderView {
            generation: gpu.resource_generation,
            anchor: [0.; 3],
            regions: &regions,
        })?;
        let t = gpu.scene_timing().unwrap();
        if t.build_quads != 0 {
            batches.push(t.build_quads);
        }
        if done {
            break;
        }
        assert_eq!(gpu.scene.top, top);
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
        std::thread::yield_now();
    }
    assert_eq!(batches, [80_000, 80_000]);
    assert_eq!(gpu.region_cache.entries().len(), 2);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；移动未变分片只重定位已有的面和驻留身份"]
fn streamed_fragment_reuses_encoding_after_an_earlier_fragment_grows()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let output = image(&mut gpu)?;
    let q = scene().quads[0];
    let mut tail = vec![q; 4097];
    tail[4096] = Quad::with_material(q.positions(), q.uv(), q.tint(), MaterialId::new(1));
    let mut parts = prime_scene::MeshParts::default();
    parts.insert(0, Mesh { quads: vec![q] })?;
    parts.insert(19, Mesh { quads: tail })?;
    let drive = |gpu: &mut Gpu,
                 parts: &prime_scene::MeshParts,
                 geometry: u64,
                 content: u64|
     -> Result<(u64, u64, u64), Box<dyn std::error::Error>> {
        let start = std::time::Instant::now();
        let mut stats = (0, 0, 0);
        loop {
            let ready = gpu.stream_regions(RenderView {
                generation: gpu.resource_generation,
                anchor: [0.; 3],
                regions: &[RenderRegion {
                    key: [0; 3],
                    geometry_revision: geometry,
                    content_revision: content,
                    mesh: parts.view(),
                }],
            })?;
            let t = gpu.scene_timing().unwrap();
            stats.0 += t.encoded_quads;
            stats.1 += t.reused_fragment_quads;
            stats.2 += t.cpu_source_bytes;
            if ready {
                return Ok(stats);
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(15));
            std::thread::yield_now();
        }
    };
    let check_lamp = |gpu: &mut Gpu, local: u32| -> Result<(), Box<dyn std::error::Error>> {
        host.render_environment(gpu, &output, &camera(), Environment::default())?;
        gpu.complete_previous()?;
        let mut meta = [0u32; 4];
        gpu.lighting_gpu
            .meta
            .read(&mut gpu.context, 0, bytemuck::bytes_of_mut(&mut meta))?;
        assert_eq!(meta[3], gpu.region_cache.entries()[0].range.start + local);
        Ok(())
    };
    assert_eq!(drive(&mut gpu, &parts, 1, 1)?, (4098, 0, 4098 * 96));
    check_lamp(&mut gpu, 4097)?;
    let id = gpu.region_cache.entries()[0].resident.token(1);
    parts.insert(0, Mesh { quads: vec![q; 3] })?;
    assert_eq!(drive(&mut gpu, &parts, 2, 2)?, (3, 4097, 3 * 96));
    check_lamp(&mut gpu, 4099)?;
    assert_eq!(gpu.region_cache.entries()[0].resident.token(3), id);
    assert_ne!(gpu.region_cache.entries()[0].resident.token(1), id);
    let top = gpu.scene.top;
    let recolored = Quad::with_material(q.positions(), q.uv(), [0.25, 0.5, 1.], q.material_id());
    parts.insert(
        0,
        Mesh {
            quads: vec![recolored; 3],
        },
    )?;
    assert_eq!(drive(&mut gpu, &parts, 2, 3)?, (0, 4097, 3 * 96));
    check_lamp(&mut gpu, 4099)?;
    assert_eq!(gpu.scene.top, top);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；快速移动时完成在途候选，再独立移动已发布 TLAS"]
fn streamed_candidate_finishes_while_anchor_keeps_moving() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.set_terrain_upload_budget(32)?;
    let mesh = Mesh {
        quads: vec![scene().quads[0]; 128],
    };
    let regions = [RenderRegion {
        key: [0; 3],
        geometry_revision: 1,
        content_revision: 1,
        mesh: (&mesh).into(),
    }];
    let start = std::time::Instant::now();
    let mut target = [0.; 3];
    let mut pinned = None;
    loop {
        let ready = gpu.stream_regions(RenderView {
            generation: gpu.resource_generation,
            regions: &regions,
            anchor: target,
        })?;
        assert!(gpu.scene_timing().unwrap().terrain_upload_bytes <= 32);
        if ready {
            break;
        }
        if let Some(storage) = &gpu.pending_storage {
            assert_eq!(*pinned.get_or_insert(storage.anchor), storage.anchor);
            target[0] += 64.;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        std::thread::yield_now();
    }
    assert_eq!(Some(gpu.scene_anchor()), pinned);
    assert_ne!(gpu.scene_anchor(), target);
    let blas = gpu.region_cache.entries()[0].blas;
    publish_stream(&mut gpu, &mesh, 1, 1, target)?;
    assert_eq!(gpu.scene_anchor(), target);
    assert_eq!(gpu.region_cache.entries()[0].blas, blas);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

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
        opacity_quads: &[],
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
#[ignore = "真实 Vulkan；混合格式的紧凑面与回退面按源序逐位展开"]
fn mixed_quad_encodings_preserve_source_vertices() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let positions = [
        [
            [241., 4., 0.],
            [244., 4., 2.],
            [244., 7., 2.],
            [241., 7., 0.],
        ],
        [[4., 2., 0.], [5., 2., 0.], [5., 3., 1.], [4., 3., 1.]],
        [
            [200., 4., 2.],
            [203., 7., 3.],
            [207., 9., 1.],
            [199., 8., 2.],
        ],
        [
            [-1.5, 0., 0.],
            [-0.5, 0., 0.],
            [-0.5, 1., 0.],
            [-1.5, 1., 0.],
        ],
        [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
        [[0.1, 0., 0.], [1., 0., 0.], [1., 1., 0.], [0.1, 1., 0.]],
    ];
    let mesh = Mesh {
        quads: positions
            .into_iter()
            .map(|p| Quad::with_material(p, [[0.; 2]; 4], [1.; 3], MaterialId::new(0)))
            .collect(),
    };
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    let range = gpu.region_cache.entries()[0].range.clone();
    let bytes = u64::from(range.end) * 64;
    let readback = Buffer::new(&mut gpu.context, bytes, true)?;
    gpu.known_geometry
        .unwrap()
        .copy_prefix_to(&mut gpu.context, &readback, bytes)?;
    let mut vertices = vec![[[0u32; 4]; 4]; positions.len()];
    readback.read(
        &mut gpu.context,
        u64::from(range.start) * 64,
        bytemuck::cast_slice_mut(&mut vertices),
    )?;
    for (quad, expected) in vertices.iter().zip(positions) {
        for (actual, p) in quad.iter().zip(expected) {
            assert_eq!(actual[..3], p.map(f32::to_bits));
        }
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；分帧上传完整后发布并验证驻留身份"]
fn streamed_region_preserves_resident_quad_ids() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.set_terrain_upload_budget(32)?;
    let output = image(&mut gpu)?;
    let make = |cell: u32| {
        let x = (cell % 32) as f32;
        let y = (cell / 32) as f32;
        Quad::with_material(
            [
                [x, y, 1.],
                [x + 1., y, 1.],
                [x + 1., y + 1., 1.],
                [x, y + 1., 1.],
            ],
            [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            [1.; 3],
            MaterialId::new(0),
        )
    };
    let mut mesh = Mesh {
        quads: (0..69).rev().map(make).collect(),
    };
    let upload = |gpu: &mut Gpu,
                  mesh: &Mesh,
                  revision: u64|
     -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        let top = gpu.scene.top;
        let regions = [RenderRegion {
            key: [0; 3],
            geometry_revision: revision,
            content_revision: revision,
            mesh: mesh.into(),
        }];
        let mut published = false;
        for frame in 0..512 {
            let ready = gpu.stream_regions(RenderView {
                generation: gpu.resource_generation,
                regions: &regions,
                anchor: [0.; 3],
            })?;
            let timing = gpu.scene_timing().unwrap();
            assert!(timing.terrain_upload_bytes <= 32);
            if ready {
                assert!(frame > 0);
                published = true;
                break;
            }
            assert!(timing.upload_pending);
            assert_eq!(gpu.scene.top, top);
            if frame == 0 {
                host.render_environment(gpu, &output, &camera(), Environment::default())?;
            }
        }
        assert!(published);
        host.render_environment(gpu, &output, &camera(), Environment::default())?;
        gpu.complete_previous()?;
        let range = gpu.region_cache.entries()[0].range.clone();
        let mut result = vec![0u64; range.len()];
        gpu.scene.tokens.read(
            &mut gpu.context,
            u64::from(range.start) * 8,
            bytemuck::cast_slice_mut(&mut result),
        )?;
        Ok(result)
    };
    let probe = |gpu: &mut Gpu| -> Result<[[u32; 4]; 2], Error> {
        let (mut bindings, _) = gpu.renderer_bindings(
            gpu.view.as_ref().unwrap(),
            &gpu.atlas,
            &gpu.scene,
            gpu.material_gpu,
            gpu.lighting_gpu,
        );
        let read = Buffer::new(&mut gpu.context, 32, true)?;
        bindings.push(Binding::StorageBuffer(read));
        let bindings = Bindings::new(&mut gpu.context, &bindings)?;
        let pipe = ComputePipeline::new(
            &mut gpu.context,
            bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/triangle_identity_probe.spv")),
                c"triangle_identity_probe_main",
            ),
        )?;
        let mut cmds = Commands::new(&gpu.context)?;
        cmds.dispatch(pipe, [1; 3])?;
        cmds.finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        let mut words = [[0; 4]; 2];
        read.read(&mut gpu.context, 0, bytemuck::cast_slice_mut(&mut words))?;
        Ok(words)
    };
    let old = upload(&mut gpu, &mesh, 1)?;
    assert!(old.windows(2).all(|pair| pair[0] + 1 == pair[1]));
    let hits = probe(&mut gpu)?;
    assert_eq!(hits[0][3], 1);
    assert_eq!(hits[1][3], 1);
    assert_eq!(&hits[0][..2], &hits[1][..2]);
    assert_ne!(hits[0][2], hits[1][2]);
    mesh.quads.reverse();
    mesh.quads[3] = make(99);
    let new = upload(&mut gpu, &mesh, 2)?;
    assert_eq!(hits, probe(&mut gpu)?);
    for (i, &id) in new.iter().enumerate() {
        if i == 3 {
            assert!(!old.contains(&id));
        } else {
            assert_eq!(id, old[old.len() - 1 - i]);
        }
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；白名单回退与默认材质发布相同的GPU参数和操作槽"]
fn hardware_pt_unsupported_recipe_publishes_default_material()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.experiment_dispatch = 8;
    let default = MaterialDefinition::default();
    let mut unsupported = default;
    unsupported.control[0] = 2;
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.5; 2], [0.8; 3], 4.);
    upload(
        &mut gpu,
        &[MaterialDefinition::default()],
        &[],
        &[[u32::MAX, 0, 0, 0]],
    )?;
    {
        gpu.clear_world()?;
        gpu.set_rendering(prime_scene::RenderMode::Realtime)?;
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
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；局部Guide源码重载必须替换实际程序且保留未变化的照明程序"]
fn hardware_pt_guide_source_reload_replaces_the_executed_program()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = root.join("../../target/guide-reload-tests").join(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let sources = scratch.join("sources");
    std::fs::create_dir_all(&sources)?;
    for entry in std::fs::read_dir(gpu.shader_source.parent().unwrap())? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), sources.join(entry.file_name()))?;
        }
    }
    let engine = root.join(env!("PRIME_ENGINE_SHADERS_RELATIVE"));
    gpu.compiler =
        shader_build::RuntimeCompiler::new(scratch.join("cache"), &[&sources, &engine], &[])?;
    gpu.shader_source = sources.join("primary.slang");
    let definitions = [MaterialDefinition::default()];
    let areas = [[u32::MAX, 0, 0, 0]];
    upload(&mut gpu, &definitions, &[], &areas)?;
    let generation = gpu.generation.as_ref().unwrap();
    let before = generation
        .guide_regular
        .unwrap()
        .program_root(&gpu.context)?;
    let pt = generation.code[0];
    std::fs::write(
        sources.join("guides.slang"),
        "module guides; [[vk::binding(19, 0)]] [[vk::image_format(\"r32f\")]] RWTexture2D<float> probe_depth; [shader(\"raygeneration\")] void guide_main() { probe_depth[DispatchRaysIndex().xy] = 123.0; }",
    )?;
    upload(&mut gpu, &definitions, &[], &areas)?;
    let generation = gpu.generation.as_ref().unwrap();
    assert_ne!(
        before,
        generation
            .guide_regular
            .unwrap()
            .program_root(&gpu.context)?
    );
    assert_eq!(pt, generation.code[0]);
    let guide = generation.guide_regular.unwrap();
    let view = View::new(&mut gpu.context, [32, 32], true)?;
    let descriptors = gpu.renderer_bindings(
        &view,
        &gpu.atlas,
        &gpu.scene,
        gpu.material_gpu,
        gpu.lighting_gpu,
    );
    let (bindings, _) = gpu.allocate_bindings(descriptors)?;
    let guide = guide.rebind(&mut gpu.context, bindings)?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.trace(guide, [32, 32])?;
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let depth = view.rr.images[1].download(&mut gpu.context)?;
    assert!(
        depth
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| f32::from_le_bytes(*pixel) == 123.)
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独立进程：实际SDK+EXT验证生产RR选择keyed Guide，原生程序不保留Guide SBT"]
fn hardware_pt_rr_keyed_program_preserves_opaque_and_glass_guides()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(true, true)?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
        assert_eq!(generation.guide_regular.is_some(), mode == 8);
        assert_eq!(generation.guide_reordered.is_some(), mode == 8);
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
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；超过八种材质能力仍可执行PT的特化材质程序"]
fn hardware_pt_many_material_variants() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    gpu.set_rendering(prime_scene::RenderMode::Realtime)?;
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
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan compute；以两个基址验证全局图元索引跨过24位"]
fn hardware_pt_region_primitive_index_keeps_full_quad_base()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
#[ignore = "真实Vulkan RT；纯tint只写变化quad且整批拒绝不部分写入"]
fn hardware_pt_sparse_tint_publication_keeps_geometry_and_writes_only_the_patch()
-> Result<(), Box<dyn std::error::Error>> {
    use prime_scene::{FragmentMask, MeshParts, PreparedParts, RenderChange, TintPatch};
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
        simple_engine::color::Srgb8Rgba::new(0x12, 0x34, 0x56, 255)
            .to_linear_srgb()
            .to_array()
            .map(f32::to_bits)[..3]
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let initial_bytes = gpu.scene_timing().unwrap().terrain_upload_bytes;
    mesh.quads[0].set_tint([0.2, 0.8, 0.4]);
    submit(&mut gpu, &mesh, 2, [0.; 3])?;
    assert_eq!(gpu.scene.quads, storage);
    assert_eq!(gpu.scene.top, top);
    assert_eq!(gpu.region_cache.entries()[0].blas, blas);
    let timing = gpu.scene_timing().unwrap();
    assert_eq!(timing.attribute_upload_bytes, 96);
    assert!(timing.terrain_upload_bytes < initial_bytes);
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    // 两条实例目录记录经过主机，新增面的属性由压紧几何在 GPU 生成。
    let update = gpu.scene_timing().unwrap();
    assert_eq!(update.attribute_upload_bytes, 8);
    assert!(update.batch.input_bytes >= 4096 * 8);
    let added = u64::from(gpu.region_cache.entries()[1].range.start) * 48;
    let mut first = [0; 48];
    gpu.scene.quads.read(&mut gpu.context, added, &mut first)?;
    assert_eq!(first[..], before[..48]);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；看不到灯的接收面通过NEE变亮，单/双面发光遵守同一PDF"]
fn hardware_pt_emissive_quad_lights_the_receiver_from_the_correct_side()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    // EV 不重置生产绝对样本身份；线性倍率对照须显式复用同一组随机样本。
    gpu.test_restart_sample_sequence(0)?;
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
            opacity_quads: &[],
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    assert_eq!(gpu.lighting_gpu.roots()[1..], lighting.roots()[1..]);
    mesh.quads.push(quad);
    publish(&mut gpu, &mesh, 2, 3, [0.; 3])?;
    assert_eq!(gpu.lighting_gpu.roots()[1..], lighting.roots()[1..]);
    let mut bytes = vec![0; lighting.selection.len() as usize];
    lighting.selection.read(&mut gpu.context, 0, &mut bytes)?;
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
    assert_eq!(gpu.lighting_gpu.roots()[1..], lighting.roots()[1..]);
    assert_light_pdfs(&host, &mut gpu, &image, "one_emitter_moved")?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；多组灯跨越 64 项排序块时，空间重排仍能按原 rank 反查"]
fn hardware_pt_spatial_light_merge_keeps_inverse_paths() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let lamp = MaterialDefinition {
        emission: [1., 1., 1., 20.],
        ..Default::default()
    };
    upload(&mut gpu, &[lamp], &[], &[[u32::MAX, 0, 0, 0]])?;
    let prototype = scene().quads[1];
    let count = 149usize;
    let mesh = Mesh {
        quads: (0..count)
            .map(|i| {
                let x = (i * 71 % count) as f32 * 0.125;
                Quad::with_material(
                    prototype.positions().map(|p| [p[0] + x, p[1], p[2]]),
                    prototype.uv(),
                    prototype.tint(),
                    MaterialId::new(0),
                )
            })
            .collect(),
    };
    publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    assert_light_pdfs(&host, &mut gpu, &image, "149_spatial_leaves")?;
    let buffer = gpu.lighting_gpu.tree;
    let readback = Buffer::new(&mut gpu.context, buffer.len(), true)?;
    buffer.copy_prefix_to(&mut gpu.context, &readback, buffer.len())?;
    let mut words = vec![0u32; buffer.len() as usize / 4];
    readback.read(&mut gpu.context, 0, bytemuck::cast_slice_mut(&mut words))?;
    let leaves = ((count - 1) * 10).next_multiple_of(8);
    let mut ordered = Vec::new();
    for rank in 0..count {
        let leaf = &words[leaves + 8 * rank..];
        let encoded = leaf[7];
        let depth = encoded >> 27;
        let path = (encoded & ((1 << 27) - 1)).reverse_bits();
        assert!((7..=8).contains(&depth));
        ordered.push((path, f32::from_bits(leaf[0])));
    }
    ordered.sort_unstable_by_key(|p| p.0);
    assert!(
        ordered
            .windows(2)
            .all(|p| p[0].0 < p[1].0 && p[0].1 < p[1].1)
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；共面异材质双面保留各自属性与资源期callable"]
fn hardware_pt_bilateral_distinct_materials_select_the_visible_front()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
        opacity_quads: &[],
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
    publish_stream(&mut gpu, &mesh, 1, 1, [0.; 3])?;
    assert!(gpu.region_cache.entries()[0].opaque);
    let before = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    mesh.quads[0] = quad(4., 2);
    publish_stream(&mut gpu, &mesh, 1, 2, [0.; 3])?;
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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
        opacity_quads: &[],
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
    authored_normal_light_support(false)
}

#[test]
#[ignore = "真实EXT/Vulkan RT；缓存首命中重建与直接命中保留相同法线贴图光照支持"]
fn hardware_pt_primary_cache_preserves_authored_normal_light_support()
-> Result<(), Box<dyn std::error::Error>> {
    authored_normal_light_support(true)
}

fn authored_normal_light_support(staged: bool) -> Result<(), Box<dyn std::error::Error>> {
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
        let mut session = host.gpu()?;
        let mut gpu = session.gpu();
        gpu.experiment_dispatch = dispatch;
        let output = image(&mut gpu)?;
        let mut receiver = MaterialDefinition {
            base_roughness: [0.8, 0.8, 0.8, 1.],
            ..MaterialDefinition::default()
        };
        if staged {
            // 消除首面提案概率差异，只比较缓存重建和直接命中的物理输入。
            receiver.surface[0] = 1.;
        }
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
            &[230, 128, 0, if staged { 0 } else { 85 }],
        )?;
        publish(&mut gpu, &mesh, 1, 1, [0.; 3])?;
        gpu.set_ser(true)?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(!staged))?;
        gpu.test_restart_sample_sequence(0)?;
        host.render_options(
            &mut gpu,
            &output,
            &camera(),
            Environment {
                sun_direction: [0.; 3],
                ..Default::default()
            },
            Options::new(
                false,
                streamline::Quality::Dlaa,
                if staged { 1 } else { 16 },
                1,
            )?,
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
                let error = pixels
                    .iter()
                    .zip(&reference)
                    .flat_map(|(actual, expected)| {
                        (0..4).map(move |channel| {
                            (actual[channel] - expected[channel]).abs()
                                / expected[channel].abs().max(1.)
                        })
                    })
                    .fold(0f32, f32::max);
                println!("normal support staged={staged} maximum scaled error={error}");
                assert!(
                    error <= 5e-5,
                    "mode={dispatch}, maximum scaled error={error}"
                );
            }
        } else {
            assert!(
                center_mean <= 1e-8,
                "flat normal must reject the same backside light: {center_mean}"
            );
        }
        session.finish()?;
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "原生 Vulkan 精确二值 OMM；主查询和阴影对照软件覆盖，不启动 Minecraft"]
fn hardware_pt_exact_opacity_matches_software_and_rebinds_uv()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    if !host.opacity_micromap {
        return Err("设备缺少 KHR OMM，不能计为通过".into());
    }
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    gpu.experiment_dispatch = 8;
    let mut cutout = MaterialDefinition::default();
    cutout.control[2] = MaterialDefinition::CUTOUT;
    let mut front = cutout;
    front.control[2] |= MaterialDefinition::FRONT_ONLY;
    let definitions = [cutout, front];
    let normal = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]];
    let rotated = [[1., 0.], [1., 1.], [0., 1.], [0., 0.]];
    let shifted = [[0.01, 0.], [1., 0.], [1., 1.], [0.01, 1.]];
    let mut mesh = Mesh::default();
    let mut prototypes = Vec::new();
    for (index, uv) in [normal, rotated, shifted, normal].into_iter().enumerate() {
        let x = index as f32 * 3.;
        let material = if index == 3 { 1 } else { 0 };
        prototypes.push(simple_engine::opacity::Quad { material, uv });
        mesh.quads.push(Quad::with_material(
            [
                [x - 1., -1., 1.],
                [x + 1., -1., 1.],
                [x + 1., 1., 1.],
                [x - 1., 1., 1.],
            ],
            uv,
            [1.; 3],
            MaterialId::new(material),
        ));
    }
    let mut reference = None;
    for enabled in [false, true] {
        gpu.replace_resources(ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [2, 2],
            albedo: &[
                255, 255, 255, 127, 255, 255, 255, 128, 255, 255, 255, 255, 255, 255, 255, 0,
            ],
            materials: &definitions,
            mers: &[0; 16],
            auxiliary: &[128; 16],
            emission_alias_words: &[],
            light_areas: &[[u32::MAX, 0, 0, 0]; 2],
            opacity_quads: if enabled { &prototypes } else { &[] },
        })?;
        assert_eq!(gpu.opacity.map.is_some(), enabled);
        publish(gpu, &mesh, 1, 1, [0.; 3])?;
        let actual = opacity_probe(gpu)?;
        assert_eq!(&actual[..64], &actual[64..]);
        assert!(actual.contains(&0) && actual.contains(&1));
        if let Some(expected) = &reference {
            assert_eq!(&actual, expected);
        } else {
            reference = Some(actual);
        }
    }
    let old = gpu.region_cache.entries()[0].blas;
    let positions = mesh.quads[0].positions();
    mesh.quads[0] = Quad::with_material(positions, rotated, [1.; 3], MaterialId::new(0));
    publish(gpu, &mesh, 1, 2, [0.; 3])?;
    assert_ne!(gpu.region_cache.entries()[0].blas, old);
    let changed = opacity_probe(gpu)?;
    assert_ne!(&changed[..16], &reference.as_ref().unwrap()[..16]);
    assert_eq!(&changed[..64], &changed[64..]);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn opacity_probe(gpu: &mut Gpu) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    let output = Buffer::new(&mut gpu.context, 128 * 4, true)?;
    let mut descriptors = vec![
        Binding::Acceleration(gpu.scene.top),
        Binding::StorageBuffer(gpu.scene.quads),
        Binding::SampledImage(gpu.atlas),
        Binding::Sampler(gpu.nearest),
        Binding::StorageBuffer(output),
    ];
    descriptors.resize(27, Binding::StorageBuffer(output));
    descriptors.extend([
        Binding::StorageBuffer(gpu.material_gpu.table),
        Binding::SampledImage(gpu.material_gpu.mers),
        Binding::SampledImage(gpu.material_gpu.auxiliary),
    ]);
    descriptors.resize(36, Binding::StorageBuffer(output));
    descriptors.push(Binding::StorageBuffer(gpu.scene.instance_quad_bases));
    let bindings = Bindings::new(&mut gpu.context, &descriptors)?;
    let source = gpu.shader_source.with_file_name("opacity_test.slang");
    let compile = |entry, stage| {
        gpu.compiler.compile(&shader_build::Recipe {
            source: source.to_str().unwrap(),
            entry,
            stage,
            output: "opacity-test.spv",
            target: shader_build::Target::Spirv("spirv_1_4"),
            args: &[],
        })
    };
    let trace = compile("trace_main", "raygeneration")?;
    let query = compile("query_main", "compute")?;
    let programs = programs(&[], false, gpu.generation.as_ref().unwrap().hit);
    let trace = RayTracingPipeline::new(
        &mut gpu.context,
        bindings,
        [
            RayShader {
                spirv: trace.bytes(),
                entry: c"trace_main",
            },
            programs.miss,
            programs.closest_hit,
        ],
        programs.any_hit,
        &[],
    )?;
    let query = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        RayShader {
            spirv: query.bytes(),
            entry: c"query_main",
        },
    )?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.trace(trace, [64, 1])?;
    commands.barrier();
    commands.dispatch(query, [1, 1, 1])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let mut bytes = vec![0u8; 128 * 4];
    output.read(&mut gpu.context, 0, &mut bytes)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .collect())
}

#[test]
#[ignore = "真实 Vulkan；静止相机下局部拆除屋顶分批刷新，不清空体积光"]
fn local_roof_edit_refreshes_volume_while_camera_is_stationary()
-> Result<(), Box<dyn std::error::Error>> {
    use simple_engine::pt::history::{Histories, History, Invalidation};
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    exposure_view(gpu)?;
    let ray = Camera {
        right: [0.005, 0., 0., 0.],
        up: [0., 0.005, 0., 0.],
        ..camera()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 1)?;
    let env = Environment {
        sun_direction: [0., 1., 0.],
        ..Default::default()
    };
    let mut mesh = Mesh {
        quads: vec![Quad::with_material(
            [
                [-512., 40., -128.],
                [-512., 40., 2048.],
                [512., 40., 2048.],
                [512., 40., -128.],
            ],
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(0),
        )],
    };
    let render = |gpu: &mut Gpu| -> Result<f32, Box<dyn std::error::Error>> {
        gpu.invalidate_histories(Invalidation::new(
            Histories::one(History::Accumulation),
            GlobalReset::ExplicitRequested,
        ))?;
        gpu.absolute_sample = 0;
        host.render_options(gpu, &output, &ray, env, options)?;
        let rgb = accumulated_rgb(gpu)?;
        Ok(rgb.iter().map(|p| p[1]).sum::<f32>() / rgb.len() as f32)
    };
    publish(gpu, &mesh, 1, 1, [0.; 3])?;
    let shade = render(gpu)?;
    mesh.quads[0].translate([6000., 0., 0.]);
    publish(gpu, &mesh, 2, 2, [0.; 3])?;
    let first = render(gpu)?;
    let mut clear = first;
    for _ in 1..16 {
        clear = render(gpu)?;
    }
    assert!(
        first < clear && shade < clear,
        "遮挡 {shade}，首帧 {first}，收敛 {clear}"
    );
    gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
    let rebuilt = render(gpu)?;
    assert!(
        (clear - rebuilt).abs() < 0.001 * (1. + clear),
        "自然刷新 {clear}，全量 {rebuilt}"
    );
    eprintln!("体积光静止编辑，旧值 {shade}，首帧 {first}，自然收敛 {clear}，显式重建 {rebuilt}");
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn black_cave_material() -> MaterialDefinition {
    let mut material = MaterialDefinition::default();
    material.control[2] = 0;
    material.base_roughness = [0., 0., 0., 1.];
    material.surface[0] = 1.;
    material
}

fn cave_wall(points: [[f32; 3]; 4]) -> Quad {
    Quad::with_material(points, [[0.5; 2]; 4], [0.; 3], MaterialId::new(0))
}

fn volume_peak(
    host: &Host,
    gpu: &mut Gpu,
    output: &Image,
    ray: &Camera,
    sun: Environment,
) -> Result<f32, Box<dyn std::error::Error>> {
    use simple_engine::pt::history::{Histories, History, Invalidation};
    gpu.invalidate_histories(Invalidation::new(
        Histories::one(History::Accumulation),
        GlobalReset::ExplicitRequested,
    ))?;
    gpu.absolute_sample = 0;
    host.render_options(
        gpu,
        output,
        ray,
        sun,
        Options::new(false, streamline::Quality::Dlaa, 1, 1)?,
    )?;
    let pixels = accumulated_rgb(gpu)?;
    assert!(pixels.iter().flatten().all(|v| v.is_finite()));
    Ok(pixels.into_iter().flatten().fold(0., f32::max))
}

#[test]
#[ignore = "真实 Vulkan；封闭无光洞穴的有限空气段不产生散射"]
fn sealed_cave_has_no_solar_in_scattering() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    exposure_view(gpu)?;
    upload(gpu, &[black_cave_material()], &[], &[[u32::MAX, 0, 0, 0]])?;
    let env = Environment {
        sun_direction: [0., 1., 0.],
        ..Default::default()
    };
    for (revision, back, half_fov) in [(1, 96., 0.02), (2, 3032., 0.001)] {
        let a = [-12., -8., -8.];
        let b = [12., 8., back];
        let mut mesh = Mesh::default();
        for axis in 0..3 {
            let u = (axis + 1) % 3;
            let v = (axis + 2) % 3;
            for side in [a[axis], b[axis]] {
                let mut corners = [a; 4];
                for corner in &mut corners {
                    corner[axis] = side;
                }
                corners[1][u] = b[u];
                corners[2][u] = b[u];
                corners[2][v] = b[v];
                corners[3][v] = b[v];
                mesh.quads.push(cave_wall(corners));
            }
        }
        publish(gpu, &mesh, revision, revision, [0.; 3])?;
        gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
        let ray = Camera {
            origin: [0., 0., 32., 0.],
            right: [half_fov, 0., 0., 0.],
            up: [0., half_fov, 0., 0.],
            ..camera()
        };
        let daylight = volume_peak(&host, gpu, &output, &ray, env)?;
        gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
        let oblique = volume_peak(
            &host,
            gpu,
            &output,
            &ray,
            Environment {
                sun_direction: [0.6, 0.8, 0.],
                ..env
            },
        )?;
        let unlit = volume_peak(
            &host,
            gpu,
            &output,
            &ray,
            Environment {
                sun_direction: [0.; 3],
                ..env
            },
        )?;
        eprintln!(
            "封闭洞穴 {} m，HDR 峰值，正午 {daylight:.9}，斜射 {oblique:.9}，关闭太阳 {unlit:.9}",
            back - 32.
        );
        assert!(
            unlit == 0. && daylight.max(oblique) <= 1e-7,
            "封闭洞穴出现入散射 {daylight} / {oblique}"
        );
        if revision == 2 {
            mesh.quads.remove(3); // +Y 屋顶，远处的黑色接收面保持原样。
            publish(gpu, &mesh, 3, 3, [0.; 3])?;
            gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
            let open = volume_peak(&host, gpu, &output, &ray, env)?;
            eprintln!("打开 3000 m 洞穴屋顶，HDR 峰值 {open:.9}");
            assert!(open > 0.001, "开阔空气失去了太阳散射 {open}");
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；太阳列从光源侧反向查询，遵循接收点所见的单面镂空覆盖"]
fn volume_columns_respect_receiver_facing_cutout() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    exposure_view(gpu)?;
    let mut sided = MaterialDefinition::default();
    sided.control[2] = MaterialDefinition::FRONT_ONLY | MaterialDefinition::CUTOUT;
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [2, 1],
        albedo: &[255, 255, 255, 0, 255, 255, 255, 255],
        materials: &[black_cave_material(), sided],
        mers: &[0; 8],
        auxiliary: &[0; 8],
        emission_alias_words: &[],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 2],
    })?;
    let up = [
        [-100., 8., -20.],
        [-100., 8., 100.],
        [100., 8., 100.],
        [100., 8., -20.],
    ];
    let down = [up[3], up[2], up[1], up[0]];
    let camera = Camera {
        right: [0.01, 0., 0., 0.],
        up: [0., 0.01, 0., 0.],
        ..camera()
    };
    let env = Environment {
        sun_direction: [0., 1., 0.],
        ..Default::default()
    };
    let mut peaks = [0.; 2];
    for (i, below) in [0.75, 0.25].into_iter().enumerate() {
        let mesh = Mesh {
            quads: vec![
                Quad::with_material(
                    [
                        [-12., -8., 64.],
                        [12., -8., 64.],
                        [12., 8., 64.],
                        [-12., 8., 64.],
                    ],
                    [[0.75, 0.5]; 4],
                    [0.; 3],
                    MaterialId::new(0),
                ),
                Quad::with_material(down, [[below, 0.5]; 4], [1.; 3], MaterialId::new(1)),
                Quad::with_material(up, [[1. - below, 0.5]; 4], [1.; 3], MaterialId::new(1)),
            ],
        };
        publish(gpu, &mesh, i as u64 + 1, i as u64 + 1, [0.; 3])?;
        gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
        peaks[i] = volume_peak(&host, gpu, &output, &camera, env)?;
    }
    eprintln!(
        "接收侧单面覆盖，不透明 {:.9}，镂空 {:.9}",
        peaks[0], peaks[1]
    );
    assert!(
        peaks[0] <= 1e-7 && peaks[1] > 0.0001,
        "接收侧不透明/镂空 {peaks:?}"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；太阳阻挡列相同时仍按移动后墙体的首命中更新可见空气"]
fn moving_cave_wall_refreshes_the_visible_air_segment() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    exposure_view(gpu)?;
    upload(gpu, &[black_cave_material()], &[], &[[u32::MAX, 0, 0, 0]])?;
    let ray = Camera {
        right: [0.01, 0., 0., 0.],
        up: [0., 0.01, 0., 0.],
        ..camera()
    };
    let sun = Environment {
        sun_direction: [0., 1., 0.],
        ..Default::default()
    };
    let mut mesh = Mesh {
        quads: vec![
            cave_wall([
                [-12., -8., 8.],
                [12., -8., 8.],
                [12., 8., 8.],
                [-12., 8., 8.],
            ]),
            cave_wall([
                [-100., 8., -20.],
                [-100., 8., 40.],
                [100., 8., 40.],
                [100., 8., -20.],
            ]),
        ],
    };
    publish(gpu, &mesh, 1, 1, [0.; 3])?;
    let shade = volume_peak(&host, gpu, &output, &ray, sun)?;
    // 墙面平行于竖直的太阳射线。平移只改变相机最近命中，屋顶遮挡范围相同。
    mesh.quads[0].translate([0., 0., 88.]);
    publish(gpu, &mesh, 2, 2, [0.; 3])?;
    let mut refreshed = 0.;
    for _ in 0..16 {
        refreshed = volume_peak(&host, gpu, &output, &ray, sun)?;
    }
    gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
    let rebuilt = volume_peak(&host, gpu, &output, &ray, sun)?;
    eprintln!("只移动洞壁，近墙 {shade:.9}，自然刷新 {refreshed:.9}，显式重建 {rebuilt:.9}");
    assert!(shade <= 1e-7 && rebuilt > 0.001 && (refreshed - rebuilt).abs() < 1e-6);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
