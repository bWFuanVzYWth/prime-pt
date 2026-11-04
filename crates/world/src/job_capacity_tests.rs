//! 联合容量确已吃满时，停车任务不占用全局八项准入槽。
use super::*;
use prime_minecraft::fixtures::{self, Pattern};

#[test]
fn more_than_eight_capacity_waiters_do_not_block_later_air_region() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    super::source_progress_tests::drive(&mut scene, &catalog, 0)?;
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let bytes = 1536 * size_of::<prime_scene::Quad>();
    scene.set_budget(Budget {
        resident_bytes: bytes,
        staging_bytes: bytes,
        region_bytes: bytes,
    })?;
    scene.set_work_budget(crate::WorkBudget {
        publications: 0,
        ..Default::default()
    });
    scene.invalidate_section([0; 3])?;
    let original = scene.state.find(chunk([0; 3])).unwrap();
    for _ in 0..256 {
        let requests = scene.plan(&camera, 0, 0, 4, 64)?.to_vec();
        if requests.is_empty() {
            scene.compile_sources(&catalog)?;
        } else {
            let records: Vec<_> = requests
                .iter()
                .map(|request| {
                    (
                        request.id,
                        request.section,
                        if request.section == [0; 3] {
                            Pattern::Dense
                        } else {
                            Pattern::Air
                        },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        if scene
            .state
            .replacement(original)
            .is_some_and(|resource| resource.owned_quads() == 1536)
        {
            break;
        }
    }
    assert_eq!(
        scene.ledger.owned, 3072,
        "暂不提交候选，旧新payload恰占满联合容量"
    );
    let kernels = scene.work_stats().total.mesh_slabs;
    let mut peak_waiters = 0;
    let mut air_resolved = false;
    for _ in 0..2048 {
        let requests = scene.plan(&camera, 8, 0, 4, 64)?.to_vec();
        if requests.is_empty() {
            scene.compile_sources(&catalog)?;
        } else {
            let records: Vec<_> = requests
                .iter()
                .map(|request| {
                    let region = request.section.map(|value| value.div_euclid(4));
                    let dense = region != [2, 0, 2]
                        && request.section.map(|value| value.rem_euclid(4)) == [0; 3];
                    (
                        request.id,
                        request.section,
                        if dense { Pattern::Dense } else { Pattern::Air },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        let parked: std::collections::HashSet<_> = scene
            .compact
            .ready
            .iter()
            .filter_map(|work| {
                scene.compact.groups.get(&crate::store::WorkId {
                    fragment: work.fragment / 4 * 4,
                    ..*work
                })
            })
            .collect();
        peak_waiters = peak_waiters.max(parked.len());
        air_resolved = scene
            .state
            .find(chunk([2, 0, 2]))
            .and_then(|handle| {
                scene
                    .state
                    .replacement(handle)
                    .or_else(|| scene.state.resource(handle))
            })
            .is_some_and(|resource| resource.missing_sections == 0 && resource.len() == 0);
        if peak_waiters > 8 && air_resolved {
            break;
        }
    }
    assert!(
        peak_waiters > 8,
        "应形成超过八个真正源齐备但容量停车的region"
    );
    assert!(
        air_resolved,
        "后来的全Air region仍须独立解析，不被停车任务挡住准入"
    );
    assert_eq!(scene.ledger.owned, 3072);
    assert_eq!(
        scene.work_stats().total.mesh_slabs,
        kernels,
        "无空间时不运行几何内核，Air走真实源证明"
    );
    Ok(())
}

#[test]
fn ample_memory_batches_many_slabs_without_exceeding_the_explicit_kernel_budget()
-> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(8)?;
    scene.source_generation(catalog.generation())?;
    scene.set_work_budget(crate::WorkBudget {
        mesh_slabs: 0,
        ..Default::default()
    });
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    for _ in 0..128 {
        let requests = scene.plan(&camera, 0, 0, 4, 64)?.to_vec();
        if requests.is_empty() {
            scene.compile_sources(&catalog)?;
        } else {
            let records: Vec<_> = requests
                .iter()
                .map(|request| {
                    (
                        request.id,
                        request.section,
                        if request.section.iter().all(|v| (0..4).contains(v)) {
                            Pattern::Dense
                        } else {
                            Pattern::Air
                        },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        if scene.compact.ready_len() == 256 {
            break;
        }
    }
    assert_eq!(scene.compact.ready_len(), 256);
    scene.set_work_budget(crate::WorkBudget {
        mesh_slabs: 64,
        ..Default::default()
    });
    scene.compile_sources(&catalog)?;
    let progress = scene.work_stats().current;
    assert_eq!(progress.mesh_slabs, 64);
    assert_eq!(
        progress.mesh_waves, 1,
        "充足余量的64项共享一次工作池批次，而非逐8项屏障"
    );
    assert_eq!(scene.compact.ready_len(), 192);
    Ok(())
}
