//! 真实region调度、分帧捕获与发布边界；输入均通过PRT2协议。
use super::*;

fn camera(origin: [f64; 3]) -> WorldCamera {
    WorldCamera {
        origin,
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    }
}
fn responses(requests: &[CaptureRequest]) -> Vec<u8> {
    colored(requests, None, 0x00ff_ffff, 0.)
}
fn unavailable(request: CaptureRequest) -> Vec<u8> {
    let mut bytes = responses(&[request]);
    bytes[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes
}
fn colored(requests: &[CaptureRequest], source: Option<[i32; 3]>, tint: u32, uv: f32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0x3254_5250u32.to_le_bytes());
    bytes.extend_from_slice(&(requests.len() as u32).to_le_bytes());
    for request in requests {
        bytes.extend_from_slice(&request.id.to_le_bytes());
        for value in request.section {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let nonempty = source == Some(request.section);
        bytes.extend_from_slice(&u32::from(nonempty).to_le_bytes());
        if nonempty {
            for word in [0u32, 1, 0, 0, 0] {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            bytes.extend_from_slice(&tint.to_le_bytes());
            for corner in [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]] {
                for value in [corner[0], corner[1], corner[2], uv, uv] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
    bytes
}
fn step(scene: &mut Scene, camera: &WorldCamera, max: usize) -> Result<(), Error> {
    let requests = scene.plan(camera, 4, 0, 4, max)?.to_vec();
    if !requests.is_empty() {
        scene.apply(&responses(&requests))?;
    }
    if scene.prepare()? {
        scene.commit()?;
    }
    assert!(scene.state.len() <= CAPACITY);
    Ok(())
}
fn settle(scene: &mut Scene, camera: &WorldCamera) -> Result<(), Error> {
    for _ in 0..CAPACITY * 4 {
        step(scene, camera, REQUESTS)?;
        if !scene.pending() {
            return Ok(());
        }
    }
    Err(Error::Input("region管理测试未收敛"))
}
fn one_region(scene: &mut Scene) -> Result<Vec<CaptureRequest>, Error> {
    Ok(scene.plan(&camera([0.; 3]), 0, 0, 4, REQUESTS)?.to_vec())
}
fn publish(scene: &mut Scene) -> Result<(), Error> {
    for _ in 0..128 {
        scene.plan(&camera([0.; 3]), 0, 0, 4, 0)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            return Ok(());
        }
    }
    Err(Error::Input("单region发布未收敛"))
}

#[test]
fn configured_bounds_are_complete_lod0_regions() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    scene.plan(&camera([-0.1, -64.1, 63.9]), 4, -4, 20, 0)?;
    let config = *scene.state.config();
    let bounds = config.bounds().unwrap();
    assert_eq!(bounds.minimum(), [-2, -1, -1]);
    assert_eq!(bounds.maximum(), [1, 5, 2]);
    assert_eq!(scene.target.unwrap().coords(), [-1, -2, 0]);
    let mut cursor = cm::RootCursor::new(config, scene.target.unwrap());
    let mut count = 0;
    loop {
        match cursor.advance() {
            cm::CursorStep::Item(value) => {
                assert_eq!(value.lod, 0);
                assert!(bounds.intersects(value));
                count += 1;
            }
            cm::CursorStep::Progress => {}
            cm::CursorStep::Done => break,
        }
    }
    assert_eq!(count, 54);
    Ok(())
}

#[test]
fn sixty_three_sections_wait_and_sixty_four_complete_one_region() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let requests = one_region(&mut scene)?;
    assert_eq!(requests.len(), 64);
    scene.apply(&responses(&requests[..63]))?;
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    assert!(scene.state.resource(handle).is_none());
    assert_eq!(scene.jobs.len(), 1);
    scene.apply(&colored(
        &requests[63..],
        Some(requests[63].section),
        0x00ff_ffff,
        0.,
    ))?;
    let resource = scene.state.resource(handle).unwrap();
    assert_eq!(resource.mesh.quads.len(), 1);
    assert_eq!(resource.ranges[64], 1);
    assert!(scene.jobs.is_empty());
    publish(&mut scene)?;
    assert_eq!(scene.entries().count(), 1);
    assert_eq!(scene.entries().next().unwrap().0, [0; 3]);
    Ok(())
}

#[test]
fn partial_host_capture_replays_the_same_remaining_ids() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let first = one_region(&mut scene)?;
    scene.apply(&responses(&first[..3]))?;
    let next = one_region(&mut scene)?;
    assert_eq!(next.len(), 61);
    assert!(
        next.iter()
            .zip(&first[3..])
            .all(|(a, b)| a.id == b.id && a.section == b.section)
    );
    Ok(())
}

#[test]
fn world_height_outside_sections_are_proven_empty_without_capture() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let requests = scene.plan(&camera([0.; 3]), 0, 1, 3, 64)?.to_vec();
    assert_eq!(requests.len(), 32);
    assert!(
        requests
            .iter()
            .all(|request| (1..3).contains(&request.section[1]))
    );
    scene.apply(&responses(&requests))?;
    assert!(
        scene
            .state
            .resource(scene.state.find(chunk([0; 3])).unwrap())
            .is_some()
    );
    Ok(())
}

#[test]
fn unavailable_is_a_resolved_negative_cache_distinct_from_real_empty() -> Result<(), Error> {
    let mut empty = Scene::new()?;
    let requests = one_region(&mut empty)?;
    empty.apply(&responses(&requests))?;
    let handle = empty.state.find(chunk([0; 3])).unwrap();
    assert!(empty.state.resource(handle).unwrap().available);
    let mut missing = Scene::new()?;
    let request = one_region(&mut missing)?[0];
    missing.apply(&unavailable(request))?;
    let handle = missing.state.find(chunk([0; 3])).unwrap();
    let resource = missing.state.resource(handle).unwrap();
    assert!(!resource.available);
    assert!(resource.mesh.quads.is_empty());
    assert!(missing.jobs.is_empty() && missing.captures.is_empty());
    publish(&mut missing)?;
    assert!(!missing.pending());
    Ok(())
}

#[test]
fn unload_withdraws_whole_region_and_reload_recaptures_all_sections() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let requests = one_region(&mut scene)?;
    let source = [1, 2, 3];
    scene.apply(&colored(&requests, Some(source), 0x00ff_ffff, 0.))?;
    publish(&mut scene)?;
    assert_eq!(scene.entries().count(), 1);
    scene.invalidate_section(source)?;
    let request = one_region(&mut scene)?[0];
    scene.apply(&unavailable(request))?;
    publish(&mut scene)?;
    assert_eq!(scene.entries().count(), 0);
    scene.invalidate_section(source)?;
    let requests = one_region(&mut scene)?;
    assert_eq!(requests.len(), 64);
    scene.apply(&colored(&requests, Some(source), 0x00ff_ffff, 0.))?;
    publish(&mut scene)?;
    assert_eq!(scene.entries().count(), 1);
    Ok(())
}

#[test]
fn editing_one_section_keeps_old_cover_until_full_region_replacement() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let requests = one_region(&mut scene)?;
    let source = [1, 2, 3];
    scene.apply(&colored(&requests, Some(source), 0x00ff_ffff, 0.))?;
    publish(&mut scene)?;
    let before = scene.entries().next().unwrap().1;
    scene.invalidate_section(source)?;
    let refresh = one_region(&mut scene)?;
    assert_eq!(refresh.len(), 1);
    assert_eq!(refresh[0].section, source);
    assert_eq!(scene.entries().next().unwrap().1, before);
    scene.apply(&responses(&refresh))?;
    assert_eq!(scene.entries().next().unwrap().1, before);
    publish(&mut scene)?;
    assert_eq!(scene.entries().count(), 0);
    Ok(())
}

#[test]
fn tint_and_uv_refresh_preserve_region_geometry_revision() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let requests = one_region(&mut scene)?;
    let source = [1, 2, 3];
    scene.apply(&colored(&requests, Some(source), 0x00ff_0000, 0.))?;
    publish(&mut scene)?;
    let (_, revision, mesh) = scene.entries().next().unwrap();
    assert_eq!(mesh.quads[0].positions()[0], [16., 32., 48.]);
    let tint = mesh.quads[0].tint();
    scene.invalidate_section(source)?;
    let requests = one_region(&mut scene)?;
    scene.apply(&colored(&requests, Some(source), 0x0000_ff00, 0.75))?;
    publish(&mut scene)?;
    let (_, next, mesh) = scene.entries().next().unwrap();
    assert_eq!(revision, next);
    assert_ne!(tint, mesh.quads[0].tint());
    assert_eq!(mesh.quads[0].uv(), [[0.75; 2]; 4]);
    Ok(())
}

#[test]
fn dirty_section_merge_preserves_unaffected_region_ranges() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let requests = one_region(&mut scene)?;
    let untouched = [1, 2, 3];
    scene.apply(&colored(&requests, Some(untouched), 0x00ff_0000, 0.25))?;
    publish(&mut scene)?;
    let old = scene.entries().next().unwrap().2.quads[0];
    scene.invalidate_section([0; 3])?;
    let requests = one_region(&mut scene)?;
    assert_eq!(requests.len(), 1);
    scene.apply(&colored(&requests, Some([0; 3]), 0x0000_ff00, 0.5))?;
    publish(&mut scene)?;
    let mesh = scene.entries().next().unwrap().2;
    assert_eq!(mesh.quads.len(), 2);
    assert_eq!(mesh.quads[1].blocks, old.blocks);
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    let resource = scene.state.resource(handle).unwrap();
    assert_eq!(resource.ranges[slot(untouched)], 1);
    assert_eq!(resource.ranges[slot(untouched) + 1], 2);
    Ok(())
}

#[test]
fn moving_window_preserves_an_intersection_capture_ticket() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let original = camera([0.; 3]);
    settle(&mut scene, &original)?;
    scene.invalidate_section([0; 3])?;
    let refresh = scene.plan(&original, 4, 0, 4, 1)?[0];
    let before = scene.captures[0].ticket;
    let requests = scene.plan(&camera([64., 0., 0.]), 4, 0, 4, 64)?.to_vec();
    assert!(requests.iter().any(|request| request.id == refresh.id));
    assert_eq!(
        scene
            .captures
            .iter()
            .find(|capture| capture.wire.id == refresh.id)
            .unwrap()
            .ticket,
        before
    );
    scene.apply(&responses(&[refresh]))?;
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    assert!(scene.state.replacement(handle).is_some());
    Ok(())
}

#[test]
fn outgoing_cover_waits_for_the_new_region_window() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    settle(&mut scene, &camera([0.; 3]))?;
    let outgoing = scene.state.find(chunk([-1, 0, 0])).unwrap();
    let next = camera([64., 0., 0.]);
    for _ in 0..16 {
        step(&mut scene, &next, 0)?;
    }
    assert!(scene.state.is_visible(outgoing));
    settle(&mut scene, &next)?;
    assert!(!scene.state.is_visible(outgoing));
    assert_eq!(scene.state.visible().count(), 9);
    Ok(())
}

#[test]
fn repeated_teleports_settle_with_bounded_storage() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    settle(&mut scene, &camera([0.; 3]))?;
    let mut final_camera = camera([0.; 3]);
    for index in 1..=12 {
        final_camera = camera([f64::from(index) * 512., 0., 0.]);
        for _ in 0..6 {
            step(&mut scene, &final_camera, REQUESTS)?;
        }
    }
    settle(&mut scene, &final_camera)?;
    assert_eq!(scene.state.visible().count(), 9);
    let bounds = scene.state.config().bounds().unwrap();
    assert!(
        scene
            .state
            .visible()
            .all(|(_, value, _)| bounds.intersects(value))
    );
    Ok(())
}

#[test]
fn old_world_response_cannot_complete_a_new_world_job() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let old = one_region(&mut scene)?[0];
    scene.clear();
    let new = one_region(&mut scene)?[0];
    assert_ne!(old.id, new.id);
    scene.apply(&responses(&[old]))?;
    assert_eq!(scene.jobs[0].received, 0);
    assert!(
        scene
            .captures
            .iter()
            .any(|capture| capture.wire.id == new.id)
    );
    Ok(())
}

#[test]
fn invalid_response_pairing_keeps_the_whole_batch_pending() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let mut issued = one_region(&mut scene)?;
    issued[1].section[0] += 1;
    assert!(scene.apply(&responses(&issued)).is_err());
    assert_eq!(scene.captures.len(), 64);
    assert_eq!(scene.jobs[0].received, 0);
    Ok(())
}

#[test]
fn failed_capture_retries_the_same_section_with_a_new_wire_id() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let failed = one_region(&mut scene)?[0];
    scene.fail_capture(failed.id)?;
    let retry = one_region(&mut scene)?;
    let retry = retry
        .iter()
        .find(|request| request.section == failed.section)
        .unwrap();
    assert_ne!(failed.id, retry.id);
    Ok(())
}

#[test]
fn invalidation_before_completion_rejects_the_old_capture() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let old = one_region(&mut scene)?;
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    scene.invalidate_section(old[0].section)?;
    scene.apply(&colored(&old, Some(old[0].section), 0x00ff_ffff, 0.))?;
    assert!(scene.state.resource(handle).is_none());
    let fresh = one_region(&mut scene)?;
    assert_eq!(fresh.len(), 64);
    assert!(
        fresh
            .iter()
            .all(|request| old.iter().all(|before| before.id != request.id))
    );
    scene.apply(&responses(&fresh))?;
    publish(&mut scene)?;
    assert!(scene.state.is_visible(handle));
    assert!(scene.entries().next().is_none());
    Ok(())
}

#[test]
fn excessive_view_distance_fails_without_reconfiguring_or_clearing() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    eprintln!(
        "region_adapter state_reserved_bytes={} region_resource_inline_bytes={} dirty_table_bytes={}",
        scene.state.reserved_bytes(),
        size_of::<RegionMesh>(),
        scene.masks.capacity() * size_of::<Dirty>()
    );
    one_region(&mut scene)?;
    let config = *scene.state.config();
    let wire = scene.captures[0].wire.id;
    assert!(scene.plan(&camera([0.; 3]), 512, -4, 20, 64).is_err());
    assert_eq!(*scene.state.config(), config);
    assert_eq!(scene.captures[0].wire.id, wire);
    scene.plan(&camera([0.; 3]), 32, -4, 20, 0)?;
    assert!(volume(scene.state.config().bounds().unwrap()) * 2 + 64 <= CAPACITY);
    Ok(())
}
