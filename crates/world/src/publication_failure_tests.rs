use super::*;

fn empty_packet(requests: &[CaptureRequest]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(0x3254_5250u32.to_le_bytes());
    bytes.extend((requests.len() as u32).to_le_bytes());
    for request in requests {
        bytes.extend(request.id.to_le_bytes());
        for coordinate in request.section {
            bytes.extend(coordinate.to_le_bytes());
        }
        bytes.extend(0u32.to_le_bytes());
    }
    bytes
}

#[test]
fn rejected_candidate_releases_its_ticket_and_can_retry_without_reusing_revision()
-> Result<(), Error> {
    let mut scene = Scene::with_threads(1)?;
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let requests = scene.plan(&camera, 0, 0, 4, 64)?.to_vec();
    let revision = scene.next_revision;
    // 模拟生产者契约破坏，在真实候选边界触发可恢复失败；不替换生产内核。
    scene.jobs[0]
        .tint_patches
        .push(prime_scene::TintPatch::new(0, 0, 1, 0xff00_0000)?);
    assert!(scene.apply(&empty_packet(&requests)).is_err());
    assert_eq!(scene.state.pending_requests(), 0);
    assert_eq!(scene.ledger.owned, 0);
    assert!(scene.jobs.is_empty());
    assert!(scene.next_revision > revision);
    let retry = scene.plan(&camera, 0, 0, 4, 64)?.to_vec();
    assert!(!retry.is_empty());
    assert!(
        retry
            .iter()
            .all(|request| requests.iter().all(|old| old.id != request.id))
    );
    scene.apply(&empty_packet(&retry))?;
    for _ in 0..64 {
        scene.plan(&camera, 0, 0, 4, 64)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    Ok(())
}
