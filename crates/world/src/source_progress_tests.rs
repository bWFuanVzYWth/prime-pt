//! 多region真实生产推进；避免单region夹具掩盖候选覆盖丢失。
use crate::{Error, Scene, WorldCamera};
use prime_minecraft::{
    Catalog,
    fixtures::{self, Pattern},
};

fn camera() -> WorldCamera {
    WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    }
}

pub(super) fn drive(scene: &mut Scene, catalog: &Catalog, distance: u32) -> Result<usize, Error> {
    let camera = camera();
    let mut colors = 0;
    for _ in 0..20_000 {
        let requests = scene.plan(&camera, distance, 0, 4, 64)?.to_vec();
        if requests.is_empty() {
            scene.compile_sources(catalog)?;
        } else {
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
            scene.apply_compact(&fixtures::section_packet(&records), catalog)?;
        }
        let count = scene.tint_requests().len();
        colors += count;
        if count != 0 {
            scene.apply_tints(&vec![0xff71_a94b; count])?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            return Ok(colors);
        }
    }
    Err(Error::Input("固定生产源推进未收敛"))
}

#[test]
fn edits_pass_full_capture_window_and_leave_a_background_turn() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    drive(&mut scene, &catalog, 0)?;
    let camera = camera();
    let pending = scene.plan(&camera, 32, 0, 4, 64)?;
    assert_eq!(pending.len(), 64);
    let background = pending[0];
    let edit = [0; 3];
    assert_ne!(background.section, edit);
    for turn in 0..8 {
        scene.apply_changes(&[crate::SourceChange::BlockRange {
            section: edit,
            minimum: [1; 3],
            maximum: [2; 3],
        }])?;
        // 模拟Java每轮只完成一个响应，已签发的远处请求保留原wire。
        let next = scene.plan(&camera, 32, 0, 4, 64)?[0];
        let pattern = if turn < 7 {
            assert_eq!(next.section, edit);
            Pattern::Tinted
        } else {
            assert_eq!(next.id, background.id);
            Pattern::Air
        };
        scene.apply_compact(
            &fixtures::section_packet(&[(next.id, next.section, pattern)]),
            &catalog,
        )?;
    }
    Ok(())
}

#[test]
fn only_local_block_events_pass_the_ordinary_capture_prefix() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    let camera = camera();
    let background = scene.plan(&camera, 32, 0, 4, 64)?[0];
    let far = [30, 0, 30];
    assert_ne!(background.section, far);
    let event = |section| crate::SourceChange::BlockRange {
        section,
        minimum: [1; 3],
        maximum: [2; 3],
    };
    scene.apply_changes(&[event(far)])?;
    assert_eq!(scene.plan(&camera, 32, 0, 4, 64)?[0].id, background.id);
    scene.apply_changes(&[event([0; 3])])?;
    assert_eq!(scene.plan(&camera, 32, 0, 4, 64)?[0].section, [0; 3]);
    Ok(())
}

#[test]
fn repeated_block_event_rejects_the_old_compact_reply() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    let camera = camera();
    let section = [0; 3];
    let event = crate::SourceChange::BlockRange {
        section,
        minimum: [1; 3],
        maximum: [2; 3],
    };
    scene.apply_changes(&[event])?;
    let first = scene.plan(&camera, 0, 0, 4, 1)?[0];
    scene.apply_changes(&[event])?;
    let latest = scene.plan(&camera, 0, 0, 4, 1)?[0];
    assert_ne!(first.id, latest.id);
    scene.apply_compact(
        &fixtures::section_packet(&[(first.id, section, Pattern::Dense)]),
        &catalog,
    )?;
    assert!(matches!(
        scene.compact.store.lookup(section),
        super::SourceState::Unknown
    ));
    scene.apply_compact(
        &fixtures::section_packet(&[(latest.id, section, Pattern::Air)]),
        &catalog,
    )?;
    assert!(matches!(
        scene.compact.store.lookup(section),
        super::SourceState::Air
    ));
    Ok(())
}

#[test]
fn identical_halo_reply_revalidates_prepared_geometry_without_rebuilding() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    drive(&mut scene, &catalog, 0)?;
    let camera = camera();
    let center = [0; 3];
    let handle = scene.state.find(crate::chunks::chunk(center)).unwrap();
    scene.invalidate_section(center)?;
    let request = scene.plan(&camera, 0, 0, 4, 64)?[0];
    assert_eq!(request.section, center);
    scene.apply_compact(
        &fixtures::section_packet(&[(request.id, center, Pattern::Sparse)]),
        &catalog,
    )?;
    for _ in 0..16 {
        if scene.state.replacement(handle).is_some() {
            break;
        }
        assert!(scene.plan(&camera, 0, 0, 4, 64)?.is_empty());
        scene.compile_sources(&catalog)?;
    }
    let replacement = scene.state.replacement(handle).unwrap().revision;
    let halo = [1, 0, 0];
    scene.invalidate_section(halo)?;
    let request = scene.plan(&camera, 0, 0, 4, 64)?[0];
    assert_eq!(request.section, halo);
    assert!(!scene.prepare()?);
    assert_eq!(scene.render_regions().next().unwrap().mesh.len(), 1536);

    let kernels = scene.work_stats().total.mesh_slabs;
    scene.apply_compact(
        &fixtures::section_packet(&[(request.id, halo, Pattern::Air)]),
        &catalog,
    )?;
    scene.plan(&camera, 0, 0, 4, 64)?;
    assert!(scene.prepare()?);
    scene.commit()?;
    let published = scene.render_regions().next().unwrap();
    assert_eq!(published.geometry_revision, replacement);
    assert_eq!(published.mesh.len(), 384);
    assert_eq!(scene.work_stats().total.mesh_slabs, kernels);
    Ok(())
}

#[test]
fn surrounding_air_sources_do_not_erase_a_completed_center_candidate() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(8)?;
    scene.source_generation(catalog.generation())?;
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let mut colors = 0;
    for _ in 0..20_000 {
        let requests = scene.plan(&camera, 4, 0, 4, 64)?.to_vec();
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
                            Pattern::Tinted
                        } else {
                            Pattern::Air
                        },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        let count = scene.tint_requests().len();
        colors += count;
        if count != 0 {
            scene.apply_tints(&vec![0xff71_a94b; count])?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(
        !scene.pending(),
        "jobs={:?} captures={} missing={} ready={} todo={} tint={} flags=({},{})",
        scene
            .jobs
            .iter()
            .map(|job| (
                job.region,
                job.changed.words(),
                job.compiled.words(),
                job.finished.words(),
                job.verify.words(),
                job.dirty,
                job.received,
                job.tint_mask.words()
            ))
            .collect::<Vec<_>>(),
        scene.captures.len(),
        scene.compact.store.missing().len(),
        scene.compact.ready_len(),
        scene.compact.todo.len(),
        scene.tint_requests().len(),
        scene.planner_pending,
        scene.dirty
    );
    assert!(colors >= 1352, "中心真实方块颜色已完整消费");
    assert_eq!(
        scene.render_regions().len(),
        1,
        "相邻空区不能吞掉已完成中心覆盖"
    );
    assert_eq!(
        scene
            .render_regions()
            .map(|region| region.mesh.len())
            .sum::<usize>(),
        1536
    );
    Ok(())
}

#[test]
fn unchanged_actual_source_preserves_published_vectors_without_a_kernel() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(2)?;
    scene.source_generation(catalog.generation())?;
    drive(&mut scene, &catalog, 0)?;
    let before = scene.render_regions().next().unwrap();
    let revisions = (before.geometry_revision, before.content_revision);
    let pointers: Vec<_> = before
        .mesh
        .fragments()
        .map(|(fragment, mesh)| (fragment, mesh.quads.as_ptr()))
        .collect();
    let kernels = scene.work_stats().total.mesh_slabs;
    scene.apply_changes(&[crate::SourceChange::BlockRange {
        section: [0; 3],
        minimum: [1; 3],
        maximum: [2; 3],
    }])?;
    assert_eq!(
        drive(&mut scene, &catalog, 0)?,
        0,
        "相同源不重求已发布原版tint"
    );
    let after = scene.render_regions().next().unwrap();
    assert_eq!((after.geometry_revision, after.content_revision), revisions);
    assert_eq!(
        after
            .mesh
            .fragments()
            .map(|(fragment, mesh)| (fragment, mesh.quads.as_ptr()))
            .collect::<Vec<_>>(),
        pointers
    );
    assert_eq!(scene.work_stats().total.mesh_slabs, kernels);
    Ok(())
}

#[test]
fn retint_does_not_apply_published_spans_to_an_untinted_replacement() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    drive(&mut scene, &catalog, 0)?;
    let handle = scene.state.find(crate::chunks::chunk([0; 3])).unwrap();
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let advance = |scene: &mut Scene| -> Result<usize, Error> {
        let records: Vec<_> = scene
            .plan(&camera, 0, 0, 4, 64)?
            .iter()
            .map(|request| {
                let pattern = if request.section == [0; 3] {
                    Pattern::Sparse
                } else {
                    Pattern::Air
                };
                (request.id, request.section, pattern)
            })
            .collect();
        if records.is_empty() {
            scene.compile_sources(&catalog)?;
        } else {
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        }
        let colors = scene.tint_requests().len();
        if colors != 0 {
            scene.apply_tints(&vec![0xff00_ff00; colors])?;
        }
        Ok(colors)
    };
    scene.apply_changes(&[crate::SourceChange::SectionSource([0; 3])])?;
    for _ in 0..128 {
        assert_eq!(advance(&mut scene)?, 0);
        if scene.state.replacement(handle).is_some() {
            break;
        }
    }
    assert!(scene.state.replacement(handle).is_some());
    assert_eq!(scene.render_regions().next().unwrap().mesh.len(), 1536);

    scene.apply_changes(&[crate::SourceChange::WorldTint])?;
    let mut tint_requests = 0;
    for _ in 0..128 {
        tint_requests += advance(&mut scene)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    assert_eq!(tint_requests, 0);
    let region = scene.render_regions().next().unwrap();
    assert_eq!(region.mesh.len(), 384);
    assert!(region.mesh.quads().all(|quad| quad.tint() == [1.; 3]));
    Ok(())
}

#[test]
fn returning_to_a_pending_unload_keeps_cover_and_applies_new_tint() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    drive(&mut scene, &catalog, 0)?;
    let original = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let away = WorldCamera {
        origin: [-64., 0., 0.],
        ..original
    };
    let value = crate::chunks::chunk([0; 3]);
    let old = scene.state.find(value).unwrap();
    scene.plan(&away, 0, 0, 4, 0)?;
    let removal = *scene
        .removes
        .iter()
        .find(|patch| patch.change == chunk_manager::Change::Remove(value))
        .expect("已签发旧覆盖的卸载");

    scene.set_work_budget(crate::WorkBudget {
        publications: 0,
        ..crate::WorkBudget::default()
    });
    scene.apply_changes(&[crate::SourceChange::WorldTint])?;
    scene.plan(&away, 0, 0, 4, 0)?;
    scene.state.validate_patch(removal.token)?;
    scene.plan(&original, 0, 0, 4, 0)?;
    if scene.prepare()? {
        scene.commit()?;
    }
    assert!(scene.state.is_visible(old));
    assert!(scene.removes.is_empty(), "取消失效卸载不消耗发布额度");

    scene.set_work_budget(crate::WorkBudget::default());
    for _ in 0..128 {
        scene.plan(&original, 0, 0, 4, 0)?;
        scene.compile_sources(&catalog)?;
        let count = scene.tint_requests().len();
        if count != 0 {
            scene.apply_tints(&vec![0xff00_ff00; count])?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        assert!(scene.state.is_visible(old));
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    let region = scene.render_regions().next().unwrap();
    assert_eq!(region.mesh.len(), 1536);
    assert!(region.mesh.quads().all(|quad| quad.tint() == [0., 1., 0.]));
    Ok(())
}

#[test]
fn probes_leave_the_window_while_real_compilation_dependencies_remain() -> Result<(), Error> {
    use chunk_manager::Bounds;
    let mut sources = super::Sources::default();
    sources.clear(1);
    sources.set_window(Bounds::new([0; 3], [4; 3])?);
    sources.store.invalidate_batch(&[[0; 3], [100; 3]])?;
    sources.refresh_source([0; 3])?;
    sources.refresh_source([100; 3])?;
    assert_eq!(sources.probes.len(), 1, "不在需求域的通知不生成捕获探针");
    let work = crate::store::WorkId {
        region: [0; 3],
        fragment: 4,
        epoch: 100,
    };
    sources.store.depend(work, &[[0; 3]])?;
    sources.set_window(Bounds::new([4, 0, 0], [8, 4, 4])?);
    assert!(sources.probes.is_empty());
    assert_eq!(
        sources.store.missing().collect::<Vec<_>>(),
        [[0; 3]],
        "仅保真实编译依赖"
    );
    sources.store.maintain(64);
    assert_eq!(sources.store.stats().sources, 1);
    sources.store.forget(work);
    sources.store.maintain(64);
    assert_eq!(sources.store.stats().sources, 0);
    Ok(())
}

#[test]
fn initial_matching_bounds_still_limit_source_only_probes_to_the_window() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    scene.plan(&camera, 0, 0, 4, 0)?;
    let far = [100, 0, 100];
    scene.apply_changes(&[crate::SourceChange::SectionSource(far)])?;
    assert!(
        scene
            .plan(&camera, 0, 0, 4, 64)?
            .iter()
            .all(|request| request.section != far)
    );
    assert!(!scene.compact.probes.contains_key(&far));
    assert!(!scene.compact.store.missing().any(|key| key == far));
    Ok(())
}
