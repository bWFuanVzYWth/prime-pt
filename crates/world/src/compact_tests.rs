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

fn unavailable_packet(request: CaptureRequest) -> Vec<u8> {
    let mut bytes = Vec::new();
    for word in [0x33545250, 1, 1, 0] {
        bytes.extend(u32::to_le_bytes(word));
    }
    bytes.extend(request.id.to_le_bytes());
    for value in request.section {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend([0u8; 16]);
    bytes
}

#[test]
fn unavailable_halo_preserves_old_mesh_until_recovered_geometry_and_tint_are_complete()
-> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xffff0000; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    let old = scene.state.resource(handle).unwrap();
    let pointer = old.parts.get(0).unwrap().quads.as_ptr();
    let revisions = (old.revision, old.content_revision);
    scene.invalidate_section([-1, 0, 0])?;
    let requests = scene.plan(&camera(), 0, 0, 4, 64)?.to_vec();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].section, [-1, 0, 0]);
    scene.apply_compact(&unavailable_packet(requests[0]), &catalog)?;
    publish(&mut scene, &catalog)?;
    let old = scene.state.resource(handle).unwrap();
    assert!(!old.available);
    assert_eq!(old.parts.get(0).unwrap().quads.as_ptr(), pointer);
    assert_eq!((old.revision, old.content_revision), revisions);
    assert_eq!(scene.render_regions().next().unwrap().mesh.len(), 1536);
    scene.invalidate_section([-1, 0, 0])?;
    let requests = scene.plan(&camera(), 0, 0, 4, 64)?.to_vec();
    let records: Vec<_> = requests
        .iter()
        .map(|request| (request.id, request.section, Pattern::Air))
        .collect();
    scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
    for _ in 0..32 {
        if scene.tint_requests().len() != 0 {
            break;
        }
        scene.plan(&camera(), 0, 0, 4, 64)?;
        scene.compile_sources(&catalog)?;
    }
    assert_ne!(scene.tint_requests().len(), 0);
    assert_eq!(
        scene
            .render_regions()
            .next()
            .unwrap()
            .mesh
            .quads()
            .next()
            .unwrap()
            .tint(),
        [1., 0., 0.]
    );
    let colors = vec![0xff00ff00; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    assert!(scene.state.resource(handle).unwrap().available);
    assert_eq!(
        scene
            .render_regions()
            .next()
            .unwrap()
            .mesh
            .quads()
            .next()
            .unwrap()
            .tint(),
        [0., 1., 0.]
    );
    Ok(())
}

#[test]
fn lowered_budget_keeps_published_partition_and_reports_the_unmet_limit() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xff123456; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    let pointer = scene
        .state
        .resource(handle)
        .unwrap()
        .parts
        .get(0)
        .unwrap()
        .quads
        .as_ptr();
    scene.set_budget(Budget {
        resident_bytes: 96,
        staging_bytes: 1537 * 96,
        region_bytes: 96,
    })?;
    scene.plan(&camera(), 0, 0, 4, 64)?;
    scene.compile_sources(&catalog)?;
    let pressure = scene.pressure();
    assert_eq!(pressure.kind, PressureKind::Resident);
    assert!(pressure.required_bytes > pressure.limit_bytes);
    assert_eq!(
        scene
            .state
            .resource(handle)
            .unwrap()
            .parts
            .get(0)
            .unwrap()
            .quads
            .as_ptr(),
        pointer
    );
    assert_eq!(scene.render_regions().next().unwrap().mesh.len(), 1536);
    assert_eq!(scene.ledger.owned, 1536);
    Ok(())
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
    let pointers: Vec<_> = before
        .mesh
        .fragments()
        .map(|(fragment, mesh)| (fragment, mesh.quads.as_ptr()))
        .collect();
    let kernels = scene.work_stats().total.mesh_slabs;
    let source_count = scene.compact.store.stats().sources;
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
    assert_eq!(scene.compact.store.stats().sources, source_count);
    let colors = vec![0xff00_ff00; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let after = scene.render_regions().next().unwrap();
    assert_eq!(after.geometry_revision, geometry);
    assert_ne!(after.content_revision, content);
    assert_eq!(
        after
            .mesh
            .fragments()
            .map(|(fragment, mesh)| (fragment, mesh.quads.as_ptr()))
            .collect::<Vec<_>>(),
        pointers
    );
    assert_eq!(
        scene.work_stats().total.mesh_slabs,
        kernels,
        "纯tint不执行几何内核"
    );
    Ok(())
}

#[test]
fn newer_tint_replaces_unpublished_tint_candidate() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let red = vec![0xffff_0000; scene.tint_requests().len()];
    scene.apply_tints(&red)?;
    publish(&mut scene, &catalog)?;

    let finish_tint = |scene: &mut Scene, color| -> Result<(), Error> {
        scene.invalidate_tints();
        for _ in 0..128 {
            assert!(scene.plan(&camera(), 0, 0, 4, 64)?.is_empty());
            scene.compile_sources(&catalog)?;
            let count = scene.tint_requests().len();
            if count != 0 {
                return scene.apply_tints(&vec![color; count]);
            }
        }
        Err(Error::Input("颜色刷新未生成回调请求"))
    };

    finish_tint(&mut scene, 0xff00_ff00)?;
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    assert!(scene.state.replacement(handle).is_some());
    assert_eq!(
        scene
            .render_regions()
            .next()
            .unwrap()
            .mesh
            .quads()
            .next()
            .unwrap()
            .tint(),
        [1., 0., 0.],
    );

    finish_tint(&mut scene, 0xff00_00ff)?;
    publish(&mut scene, &catalog)?;
    let region = scene.render_regions().next().unwrap();
    assert_eq!(region.mesh.len(), 1536);
    assert!(region.mesh.quads().all(|quad| quad.tint() == [0., 0., 1.]));
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
    assert_eq!(scene.compact.store.stats().sources, 0);
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
    assert_eq!(scene.compact.store.stats().sources, 0);
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
fn partial_section_capture_first_requests_only_the_center_source() {
    let region = [-1, -2, -3];
    let center = [-3, -7, -9];
    let keys = compact::needed(region, 1 << slot(center));
    assert_eq!(keys[0], center);
    let actual: std::collections::BTreeSet<_> = keys.iter().copied().collect();
    assert_eq!(keys.len(), 1);
    assert_eq!(actual, std::collections::BTreeSet::from([center]));
}

#[test]
fn source_retirement_reports_evicted_domain_without_retiring_live_halo() -> Result<(), Error> {
    let (mut scene, catalog) = ready_tinted()?;
    let colors = vec![0xff12_3456; scene.tint_requests().len()];
    scene.apply_tints(&colors)?;
    publish(&mut scene, &catalog)?;
    let source_count = scene.compact.store.stats().sources;
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
    let retired: Vec<_> = scene.drain_retired_sources().collect();
    assert_eq!(retired.len(), source_count);
    assert!(retired.iter().all(|key| (-1..=4).contains(&key[0])));
    assert!(matches!(
        scene.compact.store.lookup([0; 3]),
        crate::store::SourceState::Unknown
    ));
    assert!(matches!(
        scene.compact.store.lookup([64, 0, 0]),
        crate::store::SourceState::Air
    ));
    assert_eq!(scene.drain_retired_sources().count(), 0);
    Ok(())
}

fn capacity_packet(requests: &[CaptureRequest]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [0x33545250, requests.len() as u32, 1, 0] {
        bytes.extend(value.to_le_bytes());
    }
    for request in requests {
        bytes.extend(request.id.to_le_bytes());
        for value in request.section {
            bytes.extend(value.to_le_bytes());
        }
        let small = request.section == [0; 3];
        let heavy = [[-4, 0, -4], [-3, 0, -4]].contains(&request.section);
        for value in [
            1,
            u32::from(small),
            if small { 2 } else { 1 },
            if small { 64 } else { 0 },
        ] {
            bytes.extend(value.to_le_bytes());
        }
        if small {
            bytes.extend(0u32.to_le_bytes());
            bytes.extend(1u32.to_le_bytes());
            for index in 0..64 {
                bytes.extend(u64::from(index == 0).to_le_bytes());
            }
        } else {
            bytes.extend(u32::from(heavy).to_le_bytes());
        }
    }
    bytes
}

#[test]
fn capacity_blocked_large_output_does_not_stop_a_ready_small_region() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(2)?;
    scene.source_generation(catalog.generation())?;
    scene.set_budget(Budget {
        resident_bytes: 100 * 96,
        staging_bytes: 100 * 96,
        region_bytes: 100 * 96,
    })?;
    scene.set_work_budget(crate::WorkBudget {
        mesh_slabs: 0,
        ..Default::default()
    });
    for _ in 0..128 {
        let requests = scene.plan(&camera(), 1, 0, 4, 64)?.to_vec();
        if !requests.is_empty() {
            scene.apply_compact(&capacity_packet(&requests), &catalog)?;
        }
        scene.compile_sources(&catalog)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if scene.compact.ready_len() >= 12
            && scene.compact.store.missing().len() == 0
            && scene.captures.is_empty()
        {
            break;
        }
    }
    assert!(
        scene.compact.ready_len() >= 12,
        "两个大section与小section须同时可执行"
    );
    scene.set_work_budget(crate::WorkBudget {
        mesh_slabs: 64,
        ..Default::default()
    });
    scene.compile_sources(&catalog)?;
    assert!(scene.work_stats().current.mesh_slabs <= 64);
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    let ready = scene
        .state
        .replacement(handle)
        .or_else(|| scene.state.resource(handle));
    assert_eq!(
        ready.map(RegionMesh::len),
        Some(6),
        "较早的大输出受限后，同轮仍完成能容纳的小输出"
    );
    Ok(())
}
