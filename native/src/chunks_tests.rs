//! 原生区块应用适配的生命周期测试；空section同样是有效完成资源。
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
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0x3254_5250u32.to_le_bytes());
    bytes.extend_from_slice(&(requests.len() as u32).to_le_bytes());
    for request in requests {
        bytes.extend_from_slice(&request.id.to_le_bytes());
        for value in request.section {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
    }
    bytes
}

fn step(scene: &mut Scene, camera: &WorldCamera, max: usize) -> Result<(), Error> {
    let requests = scene.plan(camera, max)?.to_vec();
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
    Err(Error::Input("区块管理测试未收敛"))
}

#[test]
fn lod0_config_preserves_the_entire_section_box() -> Result<(), Error> {
    let config = configuration(initial_bounds())?;
    let target = Target::new([0; 3])?;
    let mut cursor = cm::RootCursor::new(config, target);
    let mut count = 0;
    loop {
        match cursor.advance() {
            cm::CursorStep::Item(value) => {
                assert_eq!(value.lod, 0);
                count += 1;
            }
            cm::CursorStep::Progress => {}
            cm::CursorStep::Done => break,
        }
    }
    assert_eq!(count, WINDOW);
    Ok(())
}

#[test]
fn moving_window_preserves_an_intersection_capture_ticket() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let original = camera([0.; 3]);
    settle(&mut scene, &original)?;
    let section = [0; 3];
    let handle = scene.state.find(chunk(section)).unwrap();
    scene.invalidate_section(section)?;
    let mut request = None;
    for _ in 0..64 {
        let issued = scene.plan(&original, 1)?.to_vec();
        if let Some(value) = issued.into_iter().find(|value| value.section == section) {
            request = Some(value);
            break;
        }
    }
    let request = request.expect("刷新请求必须被预算规划发现");
    scene.plan(&camera([64., 0., 0.]), REQUESTS)?;
    scene.apply(&responses(&[request]))?;
    assert_eq!(scene.state.find(chunk(section)), Some(handle));
    assert!(scene.state.replacement(handle).is_some());
    Ok(())
}

#[test]
fn outgoing_cover_waits_for_the_new_window() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    settle(&mut scene, &camera([0.; 3]))?;
    let outgoing = scene.state.find(chunk([-4, 0, 0])).unwrap();
    let next = camera([64., 0., 0.]);
    for _ in 0..16 {
        step(&mut scene, &next, 0)?;
    }
    assert!(scene.state.is_visible(outgoing));
    settle(&mut scene, &next)?;
    assert!(!scene.state.is_visible(outgoing));
    assert_eq!(scene.state.visible().count(), WINDOW);
    Ok(())
}

#[test]
fn repeated_teleports_can_still_settle_with_bounded_storage() -> Result<(), Error> {
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
    let bounds = scene.state.config().bounds().unwrap();
    assert_eq!(
        scene
            .state
            .visible()
            .filter(|(_, value, _)| bounds.intersects(*value))
            .count(),
        WINDOW
    );
    assert_eq!(scene.state.visible().count(), WINDOW);
    Ok(())
}

#[test]
fn old_world_response_cannot_complete_a_new_world_ticket() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let camera = camera([0.; 3]);
    let old = scene.plan(&camera, 1)?[0];
    scene.clear();
    let new = scene.plan(&camera, 1)?[0];
    assert_ne!(old.id, new.id);
    scene.apply(&responses(&[old]))?;
    assert_eq!(scene.state.pending_requests(), 1);
    assert_eq!(scene.captures[0].wire.id, new.id);
    Ok(())
}

#[test]
fn invalid_response_pairing_keeps_the_whole_batch_pending() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let mut issued = scene.plan(&camera([0.; 3]), 2)?.to_vec();
    assert_eq!(issued.len(), 2);
    issued[1].section[0] += 1;
    assert!(scene.apply(&responses(&issued)).is_err());
    assert_eq!(scene.state.pending_requests(), 2);
    assert_eq!(scene.captures.len(), 2);
    Ok(())
}

#[test]
fn failed_capture_can_retry_with_a_new_wire_id() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let camera = camera([0.; 3]);
    let failed = scene.plan(&camera, 1)?[0];
    scene.fail_capture(failed.id)?;
    let mut retried = None;
    for _ in 0..64 {
        let requests = scene.plan(&camera, 1)?.to_vec();
        if let Some(request) = requests
            .iter()
            .find(|request| request.section == failed.section)
        {
            retried = Some(*request);
        }
        if !requests.is_empty() {
            scene.apply(&responses(&requests))?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        if retried.is_some() {
            break;
        }
    }
    let retried = retried.expect("失败捕获必须重新签发请求");
    assert_ne!(failed.id, retried.id);
    Ok(())
}

#[test]
fn invalidation_before_completion_prevents_old_result_publication() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    let camera = camera([0.; 3]);
    let old = scene.plan(&camera, 1)?[0];
    let handle = scene.state.find(chunk(old.section)).unwrap();
    scene.invalidate_section(old.section)?;
    scene.apply(&responses(&[old]))?;
    if scene.prepare()? {
        scene.commit()?;
    }
    assert!(!scene.state.is_visible(handle));
    let mut fresh = false;
    for _ in 0..CAPACITY * 4 {
        let requests = scene.plan(&camera, REQUESTS)?.to_vec();
        fresh |= requests
            .iter()
            .any(|request| request.section == old.section && request.id != old.id);
        if !requests.is_empty() {
            scene.apply(&responses(&requests))?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        if scene.state.is_visible(handle) {
            assert!(fresh, "失效源结果不得先于刷新捕获发布");
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(fresh && scene.state.is_visible(handle) && !scene.pending());
    Ok(())
}

#[test]
fn negative_camera_selects_the_exact_half_open_lod0_box() -> Result<(), Error> {
    let mut scene = Scene::new()?;
    settle(&mut scene, &camera([-0.1, -64.1, 63.9]))?;
    let bounds = scene.state.config().bounds().unwrap();
    assert_eq!(bounds.minimum(), [-8, -12, -4]);
    assert_eq!(bounds.maximum(), [4, 0, 8]);
    assert_eq!(scene.target.unwrap().coords(), [-1, -5, 3]);
    let visible: Vec<_> = scene.state.visible().map(|(_, value, _)| value).collect();
    assert_eq!(visible.len(), WINDOW);
    assert!(
        visible
            .iter()
            .all(|value| value.lod == 0 && bounds.intersects(*value))
    );
    Ok(())
}
