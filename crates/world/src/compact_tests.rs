//! 真实PRM1/PRT3→halo→slab→tint→GPU确认边界的CPU消费者验收。
use super::*;
use prime_minecraft::Catalog;
use prime_minecraft::fixtures::{self, Pattern};

fn camera() -> WorldCamera {
    WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    }
}

fn ready_tinted() -> Result<(Scene, Catalog), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::new()?;
    scene.source_generation(catalog.generation())?;
    for _ in 0..128 {
        let requests = scene.plan(&camera(), 0, 0, 4, 64)?.to_vec();
        if !requests.is_empty() {
            let records: Vec<_> = requests
                .iter()
                .map(|request| {
                    (
                        request.id,
                        request.section,
                        if request.section == [0; 3] {
                            Pattern::Tinted
                        } else {
                            Pattern::Air
                        },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        if scene.tint_requests().len() != 0 {
            return Ok((scene, catalog));
        }
    }
    Err(Error::Input("压缩源+tint准备未收敛"))
}

fn publish(scene: &mut Scene, catalog: &Catalog) -> Result<(), Error> {
    for _ in 0..128 {
        scene.plan(&camera(), 0, 0, 4, 64)?;
        scene.compile_sources(catalog)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            return Ok(());
        }
    }
    Err(Error::Input("压缩源发布未收敛"))
}

#[test]
fn compact_tints_are_unique_and_geometry_waits_for_real_callback() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let queries: Vec<_> = scene.tint_requests().collect();
    let unique: std::collections::HashSet<_> = queries.iter().copied().collect();
    assert_eq!(queries.len(), unique.len());
    assert!(queries.len() < 1536, "同一块的多面不重复调用tint");
    assert!(
        scene
            .state
            .resource(scene.state.find(chunk([0; 3])).unwrap())
            .is_none()
    );
    let colors = vec![0xff12_3456; queries.len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let region = scene.render_regions().next().unwrap();
    assert_eq!(region.mesh.len(), 1536);
    assert_eq!(
        region.mesh.quads().next().unwrap().tint(),
        [18. / 255., 52. / 255., 86. / 255.]
    );
    Ok(())
}

#[test]
fn tint_only_invalidation_preserves_sources_and_geometry_revision() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xffff_0000; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let before = scene.render_regions().next().unwrap();
    let geometry = before.geometry_revision;
    let content = before.content_revision;
    let source_count = scene.compact.values.len();
    scene.invalidate_tints();
    for _ in 0..256 {
        assert!(
            scene.plan(&camera(), 0, 0, 4, 64)?.is_empty(),
            "纯tint不重捕palette"
        );
        scene.compile_sources(&catalog)?;
        if scene.tint_requests().len() != 0 {
            break;
        }
    }
    assert_ne!(scene.tint_requests().len(), 0);
    assert_eq!(scene.compact.values.len(), source_count);
    let colors = vec![0xff00_ff00; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let after = scene.render_regions().next().unwrap();
    assert_eq!(after.geometry_revision, geometry);
    assert_ne!(after.content_revision, content);
    Ok(())
}

#[test]
fn generation_stage_preserves_old_until_infallible_commit() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xff12_3456; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let wire = scene.next_wire;
    let prepared = scene.stage_source_generation(2)?;
    drop(prepared);
    assert_eq!(scene.generation(), 1);
    assert_eq!(scene.render_regions().len(), 1);
    scene.stage_source_generation(2)?.commit();
    assert_eq!(scene.generation(), 2);
    assert_eq!(scene.render_regions().len(), 0);
    assert_eq!(scene.next_wire, wire);
    assert!(scene.compact.values.is_empty());
    Ok(())
}

#[test]
fn unknown_state_rejects_whole_packet_before_source_cache_changes() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::new()?;
    scene.source_generation(catalog.generation())?;
    let requests = scene.plan(&camera(), 0, 0, 4, 64)?.to_vec();
    let records: Vec<_> = requests[..2]
        .iter()
        .map(|request| (request.id, request.section, Pattern::Air))
        .collect();
    let mut bytes = fixtures::section_packet(&records);
    let len = bytes.len();
    bytes[len - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(scene.apply_compact(&bytes, &catalog).is_err());
    assert!(scene.compact.values.is_empty());
    assert_eq!(scene.captures.len(), 64);
    Ok(())
}

#[test]
fn zero_remaining_payload_does_not_block_air_slabs_after_tinted_output() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xff12_3456; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let bytes = 1536 * size_of::<prime_scene::Quad>();
    scene.set_budget(Budget {
        resident_bytes: bytes,
        staging_bytes: bytes,
        region_bytes: bytes,
    })?;
    scene.invalidate_tints();
    for _ in 0..256 {
        scene.plan(&camera(), 0, 0, 4, 64)?;
        scene.compile_sources(&catalog)?;
        if scene.tint_requests().len() != 0 {
            break;
        }
    }
    assert_ne!(scene.tint_requests().len(), 0);
    assert_eq!(scene.pressure().excluded_regions, 0);
    let colors = vec![0xff65_4321; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    assert_eq!(scene.render_regions().next().unwrap().mesh.len(), 1536);
    assert_eq!(scene.pressure().kind, PressureKind::None);
    Ok(())
}

#[test]
fn partial_section_capture_requires_exact_unique_27_position_halo() {
    let region = [-1, -2, -3];
    let center = [-3, -7, -9];
    let keys = compact::needed(region, 1 << slot(center));
    assert_eq!(keys[0], center);
    let actual: std::collections::BTreeSet<_> = keys.iter().copied().collect();
    let expected: std::collections::BTreeSet<_> = (-4..=-2)
        .flat_map(|x| (-8..=-6).flat_map(move |y| (-10..=-8).map(move |z| [x, y, z])))
        .collect();
    assert_eq!(keys.len(), 27);
    assert_eq!(actual, expected);
}

#[test]
fn source_retirement_reports_evicted_domain_without_retiring_live_halo() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xff12_3456; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let camera_at = |x| WorldCamera {
        origin: [x, 0., 0.],
        ..camera()
    };
    for _ in 0..128 {
        let requests = scene.plan(&camera_at(1024.), 0, 0, 4, 64)?.to_vec();
        if !requests.is_empty() {
            let records: Vec<_> = requests
                .iter()
                .map(|r| (r.id, r.section, Pattern::Air))
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        scene.compile_sources(&catalog)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    assert_eq!(
        scene.drain_retired_sources().count(),
        0,
        "最近已解析旧域在切换前保留"
    );
    scene.plan(&camera_at(2048.), 0, 0, 4, 64)?;
    let retired: Vec<_> = scene.drain_retired_sources().collect();
    assert_eq!(retired.len(), 144);
    assert!(retired.iter().all(|key| (-1..=4).contains(&key[0])));
    assert!(
        scene
            .compact
            .values
            .keys()
            .all(|key| (63..=68).contains(&key[0]))
    );
    assert_eq!(scene.drain_retired_sources().count(), 0);
    Ok(())
}
