//! 多region真实生产推进；避免单region夹具掩盖候选覆盖丢失。
use crate::{Error, Scene, WorldCamera};
use prime_minecraft::{
    Catalog,
    fixtures::{self, Pattern},
};

pub(super) fn drive(scene: &mut Scene, catalog: &Catalog, distance: u32) -> Result<usize, Error> {
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
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
