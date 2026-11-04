//! 首次未知输入的负缓存与恢复；旧缓存已齐备的刷新用例覆盖不到此阶段。
use super::*;
use prime_minecraft::fixtures::{self, Pattern};

#[test]
fn first_unavailable_source_resolves_without_waiting_for_unread_neighbors() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let request = scene.plan(&camera, 0, 0, 4, 64)?[0];
    let mut bytes = Vec::new();
    for value in [0x33545250u32, 1, 1, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(request.id.to_le_bytes());
    for value in request.section {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend([0u8; 16]);
    scene.apply_compact(&bytes, &catalog)?;
    for _ in 0..64 {
        scene.plan(&camera, 0, 0, 4, 0)?;
        scene.compile_sources(&catalog)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(
        !scene.pending(),
        "负缓存不能等已经释放且从未读取的Unknown源"
    );
    let handle = scene.state.find(chunk([0; 3])).unwrap();
    assert!(!scene.state.resource(handle).unwrap().available);
    assert_eq!(scene.render_regions().len(), 0);
    Ok(())
}

#[test]
fn recovering_initially_unavailable_region_captures_previously_unread_sections() -> Result<(), Error>
{
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(1)?;
    scene.source_generation(catalog.generation())?;
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let request = scene.plan(&camera, 0, 0, 4, 64)?[0];
    let mut bytes = Vec::new();
    for value in [0x33545250u32, 1, 1, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(request.id.to_le_bytes());
    for value in request.section {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend([0u8; 16]);
    scene.apply_compact(&bytes, &catalog)?;
    for _ in 0..64 {
        scene.plan(&camera, 0, 0, 4, 0)?;
        scene.compile_sources(&catalog)?;
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    scene.invalidate_section(request.section)?;
    let mut saw_far = false;
    for _ in 0..256 {
        let requests = scene.plan(&camera, 0, 0, 4, 64)?.to_vec();
        saw_far |= requests.iter().any(|request| request.section == [3; 3]);
        if !requests.is_empty() {
            let records: Vec<_> = requests
                .iter()
                .map(|request| {
                    (
                        request.id,
                        request.section,
                        if request.section == [3; 3] {
                            Pattern::Dense
                        } else {
                            Pattern::Air
                        },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), &catalog)?;
        } else {
            scene.compile_sources(&catalog)?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    assert!(
        saw_far,
        "首次Unavailable不能把从未读取的其余section当作已知空"
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
