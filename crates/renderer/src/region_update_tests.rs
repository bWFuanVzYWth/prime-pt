//! 固定相机下的局部发布：真实主命中必须对应当前几何、材质和覆盖。
use super::*;
use prime_scene::{
    FragmentMask, MaterialDefinition, MaterialId, MeshParts, PreparedParts, RealtimeRenderer,
    RenderChange, RenderMode, RenderRegion, RenderView, TintPatch,
};

fn face(slot: usize, z: f32, material: u32, extra: bool) -> Mesh {
    let x = if slot & 1 == 0 { -4. } else { 0. };
    let y = if slot & 2 == 0 { -4. } else { 0. };
    let mut mesh = Mesh {
        quads: vec![Quad::with_material(
            [
                [x, y, z],
                [x + 4., y, z],
                [x + 4., y + 4., z],
                [x, y + 4., z],
            ],
            [[0.75, 0.25]; 4],
            [1.; 3],
            MaterialId::new(material),
        )],
    };
    if extra {
        mesh.quads.push(Quad::with_material(
            [[20., 20., z], [21., 20., z], [21., 21., z], [20., 21., z]],
            [[0.25; 2]; 4],
            [1.; 3],
            MaterialId::new(0),
        ));
    }
    mesh
}

fn publish(
    gpu: &mut Gpu,
    mesh: prime_scene::MeshView<'_>,
    geometry: u64,
    content: u64,
    secondary: &Mesh,
    secondary_revision: u64,
) -> Result<(), Error> {
    let region = RenderRegion {
        key: [0; 3],
        geometry_revision: geometry,
        content_revision: content,
        mesh,
    };
    let secondary = RenderRegion {
        key: [1, 0, 0],
        geometry_revision: secondary_revision,
        content_revision: secondary_revision,
        mesh: secondary.into(),
    };
    if !gpu.try_attribute_changes(
        gpu.resource_generation,
        [0.; 3],
        &[
            RenderChange::Upsert(region),
            RenderChange::Upsert(secondary),
        ],
    )? {
        gpu.set_regions(RenderView {
            generation: gpu.resource_generation,
            anchor: [0.; 3],
            regions: &[region, secondary],
        })?;
    }
    Ok(())
}

#[test]
#[ignore = "独立进程：原生4K DLAA+RR+SER+ReSTCV，固定相机局部更新后的真实命中与当前覆盖一致"]
fn hardware_pt_rr_restcv_stationary_partial_updates() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr_ser()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let extent = [3840, 2160];
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    let mut materials = [MaterialDefinition::default(); 2];
    for material in &mut materials {
        material.control = [0, 3, 0, 0];
    }
    materials[1].control[2] = MaterialDefinition::CUTOUT;
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: [2, 1],
        albedo: &[210, 100, 40, 255, 30, 180, 90, 0],
        materials: &materials,
        mers: &[0; 8],
        auxiliary: &[0; 8],
        emission_alias_words: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 2],
    })?;
    gpu.set_rendering(RenderMode::Realtime, RealtimeRenderer::RestirPt)?;
    gpu.set_render_tuning(prime_scene::RenderTuning::new(20, 0.4, false, true)?)?;
    let quality = match std::env::var("PRIME_PT_PARTIAL_QUALITY").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("dlaa") => streamline::Quality::Dlaa,
        Ok("performance") => streamline::Quality::Performance,
        _ => return Err("PRIME_PT_PARTIAL_QUALITY只接受dlaa或performance".into()),
    };
    let input_extent = if quality == streamline::Quality::Dlaa {
        extent
    } else {
        [1920, 1080]
    };
    let options = Options::new(true, quality, 1, 6)?;
    writeln!(
        std::io::stderr().lock(),
        "stationary partial output={extent:?} input={input_extent:?} quality={quality:?}"
    )?;
    let mut parts = MeshParts::default();
    for slot in 0..4 {
        parts.insert(slot, face(slot, 4., 0, false))?;
    }
    let mut background = Mesh::default();
    add_quad(&mut background, 8., [0.25; 2], [1.; 3], 32.);
    parts.insert(255, background)?;
    let secondary = face(0, 4., 0, false);
    let mut geometry = 1;
    publish(gpu, parts.view(), geometry, 1, &secondary, 1)?;
    let layout = simple_engine::restir_pt::Layout::with_color_estimates(
        input_extent,
        simple_engine::restir_pt::Fusion::DIRECTIONS,
    )?;
    let primary_readback = Buffer::new(&mut gpu.context, u64::from(layout.elements()) * 16, true)?;
    gpu.test_roots.push(primary_readback.into());
    let mut cutout = [false; 4];
    let mut heights = [4.; 4];
    let mut extras = [false; 4];
    for frame in 0..48usize {
        if frame != 0 {
            let slot = (frame / 3) % 4;
            let mut next = MeshParts::default();
            let mut changed = FragmentMask::empty();
            let tint = frame % 3 == 2;
            if !tint {
                if frame % 3 == 0 {
                    geometry += 1;
                    extras[slot] = !extras[slot];
                    heights[slot] = if extras[slot] { 4.5 } else { 4. };
                } else {
                    cutout[slot] = !cutout[slot];
                }
                next.insert(
                    slot,
                    face(slot, heights[slot], u32::from(cutout[slot]), extras[slot]),
                )?;
                changed.set(slot)?;
            }
            let candidate = PreparedParts::prepare(&parts, next, changed)?.with_tints(
                &parts,
                if tint {
                    vec![TintPatch::new(slot, 0, 1, 0xff80_c0a0)?]
                } else {
                    vec![]
                },
            )?;
            publish(
                gpu,
                candidate.view(&parts),
                geometry,
                frame as u64 + 1,
                &secondary,
                1 + frame as u64 / 4,
            )?;
            parts = candidate.finish(&mut parts);
        }
        // 先检查生产上传的数据，阻止已知无效身份继续进入路径重放。
        let positions = gpu.restir_geometry.unwrap();
        let mut data = vec![0; positions.len() as usize];
        positions.read(&mut gpu.context, 0, &mut data)?;
        for record in gpu.geometry_identities.records() {
            let [start, count, _] = record.source.location;
            for quad in start..start + count {
                let offset = 16 + quad as usize * 64 + 60;
                let token = u32::from_le_bytes(data[offset..offset + 4].try_into()?);
                assert_eq!(token, record.token, "frame={frame} 当前面身份与目录不一致");
            }
        }
        host.render_options(gpu, &output, &camera(), Environment::default(), options)?;
        assert!(gpu.rr_unavailable.is_none() && gpu.rr_previous.is_some());
        // 映射回读验证实际上传字节，不能只比较两份CPU目录。
        let entry = &gpu.region_cache.entries()[0];
        let mut actual = vec![0; parts.len() * 48];
        gpu.scene.quads.read(
            &mut gpu.context,
            u64::from(entry.range.start) * 48,
            &mut actual,
        )?;
        let expected: Vec<_> = parts.view().attributes().collect();
        assert_eq!(
            actual,
            bytemuck::cast_slice::<_, u8>(&expected),
            "frame={frame} 属性发布错位"
        );
        let primary = gpu.restir.as_ref().unwrap().primary_history();
        primary.copy_prefix_to(&mut gpu.context, &primary_readback, primary_readback.len())?;
        for (slot, &transparent) in cutout.iter().enumerate() {
            let pixel = [
                input_extent[0] * if slot & 1 == 0 { 1 } else { 3 } / 4,
                input_extent[1] * if slot & 2 == 0 { 3 } else { 1 } / 4,
            ];
            let mut bytes = [0; 16];
            primary_readback.read(
                &mut gpu.context,
                u64::from(layout.reservoir_offset(pixel).unwrap()) * 16,
                &mut bytes,
            )?;
            let hit: [u32; 4] = std::array::from_fn(|i| {
                u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
            });
            let fragment = if transparent { 255 } else { slot };
            let record = gpu
                .geometry_identities
                .records()
                .iter()
                .find(|record| record.source.key == ([0; 3], (fragment << 1) | 1))
                .unwrap();
            assert_eq!(
                hit[0], record.token,
                "frame={frame} slot={slot} 主命中丢失或覆盖错误"
            );
            assert!(hit[1] < 2, "frame={frame} slot={slot} 命中了视野外附加面");
        }
        if frame % 8 == 0 {
            writeln!(
                std::io::stderr().lock(),
                "stationary partial frame={frame} attributes=matched coverage=matched"
            )?;
        }
    }
    session.finish()?;
    writeln!(
        std::io::stderr().lock(),
        "stationary partial validation_errors={}",
        host.errors.load(Ordering::Relaxed)
    )?;
    Ok(())
}
