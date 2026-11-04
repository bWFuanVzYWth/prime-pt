//! 压缩捕获的部分可用覆盖与负缓存恢复。
use super::*;
use prime_minecraft::fixtures::{self, Pattern};

fn camera() -> WorldCamera {
    WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    }
}

fn settle(
    scene: &mut Scene,
    catalog: &Catalog,
    distance: u32,
    height: [i32; 2],
    pattern: impl Fn([i32; 3]) -> Option<Pattern>,
) -> Result<(), Error> {
    for _ in 0..1024 {
        let requests = scene
            .plan(&camera(), distance, height[0], height[1], 64)?
            .to_vec();
        if requests.is_empty() {
            scene.compile_sources(catalog)?;
        } else {
            let mut records = Vec::new();
            let mut unavailable = Vec::new();
            for request in &requests {
                if let Some(value) = pattern(request.section) {
                    records.push((request.id, request.section, value));
                } else {
                    unavailable.push(request);
                }
            }
            let mut bytes = fixtures::section_packet(&records);
            bytes[4..8].copy_from_slice(&(requests.len() as u32).to_le_bytes());
            for request in unavailable {
                bytes.extend(request.id.to_le_bytes());
                for coordinate in request.section {
                    bytes.extend(coordinate.to_le_bytes());
                }
                bytes.extend([0u8; 16]);
            }
            scene.apply_compact(&bytes, catalog)?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            return Ok(());
        }
    }
    panic!(
        "视距{distance}的压缩源发布未收敛，源缺失{}，待推进{}，建网队列{}，规划待推进{}，在途{}，节点{}，已发布{}，待发布{}，压力{:?}",
        scene.compact.store.missing().len(),
        scene.compact.todo.len(),
        scene.compact.ready_len(),
        scene.planner_pending,
        scene.jobs.len(),
        scene.state.len(),
        scene.state.visible().count(),
        scene.state.pending_patches(),
        scene.pressure()
    );
}

#[test]
fn unavailable_source_and_air_remain_distinct_at_rest() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    settle(&mut scene, &catalog, 0, [0, 4], |key| {
        (key != [0; 3]).then_some(Pattern::Air)
    })?;
    assert!(matches!(
        scene.compact.store.lookup([0; 3]),
        SourceState::Unavailable
    ));
    assert!(matches!(
        scene.compact.store.lookup([1, 0, 0]),
        SourceState::Air
    ));
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    assert_eq!(scene.state.resource(handle).unwrap().missing_sections, 1);
    assert!(scene.render_regions().next().is_none());
    Ok(())
}

#[test]
fn partial_world_height_does_not_capture_outside_sections() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    settle(&mut scene, &catalog, 8, [0, 1], |key| {
        assert_eq!(key[1], 0, "更新不能撤销世界高度外的空证明");
        Some(Pattern::Air)
    })?;
    assert_eq!(scene.state.visible().count(), 25);
    Ok(())
}

#[test]
fn expanding_distance_publishes_loaded_sections_beside_unavailable_columns() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    let edge = [31, 0, 31];
    let loaded = [29, 0, 29];
    let region = [7, 0, 7];
    let source = |key: [i32; 3]| {
        (key[0] != edge[0] || key[2] != edge[2]).then_some(if key == loaded {
            Pattern::Dense
        } else {
            Pattern::Air
        })
    };
    settle(&mut scene, &catalog, 8, [0, 4], source)?;
    settle(&mut scene, &catalog, 32, [0, 4], source)?;
    assert_eq!(
        scene
            .render_regions()
            .find(|entry| entry.key == region)
            .map(|entry| entry.mesh.len()),
        Some(1536),
        "同一合批区内已加载且邻域齐备的section必须发布"
    );

    scene.apply_changes(&[crate::SourceChange::ColumnSource {
        x: edge[0],
        z: edge[2],
        minimum_y: 0,
        maximum_y: 4,
        geometry: true,
        biome: true,
    }])?;
    settle(&mut scene, &catalog, 32, [0, 4], |key| {
        Some(if key == edge || key == loaded {
            Pattern::Dense
        } else {
            Pattern::Air
        })
    })?;
    assert_eq!(
        scene
            .render_regions()
            .find(|entry| entry.key == region)
            .map(|entry| entry.mesh.len()),
        Some(3072),
        "列加载通知补齐原来缺失的几何"
    );
    Ok(())
}
