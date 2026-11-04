use crate::{Error, Scene, WorkBudget, WorldCamera};
use prime_minecraft::fixtures::{self, Pattern};

#[test]
fn paused_mesh_tokens_still_publish_air_and_resume_without_losing_work() -> Result<(), Error> {
    let catalog = fixtures::catalog()?;
    let mut scene = Scene::with_threads(8)?;
    scene.source_generation(catalog.generation())?;
    scene.set_work_budget(WorkBudget {
        mesh_slabs: 0,
        ..WorkBudget::default()
    });
    let camera = WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let advance = |scene: &mut Scene| -> Result<(), Error> {
        let requests = scene.plan(&camera, 4, 0, 4, 64)?.to_vec();
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
        let count = scene.tint_requests().len();
        if count != 0 {
            scene.apply_tints(&vec![0xff71_a94b; count])?;
        }
        if scene.prepare()? {
            scene.commit()?;
        }
        Ok(())
    };
    for _ in 0..32 {
        advance(&mut scene)?;
    }
    let paused = scene.work_stats().total;
    assert_eq!(paused.mesh_slabs, 0);
    assert!(paused.empty_sections != 0 && paused.publications != 0);
    assert_eq!(scene.render_regions().len(), 0);
    scene.set_work_budget(WorkBudget {
        mesh_slabs: 4,
        ..WorkBudget::default()
    });
    for _ in 0..32 {
        advance(&mut scene)?;
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    assert_eq!(scene.work_stats().total.mesh_slabs, 4);
    assert_eq!(
        scene
            .render_regions()
            .map(|region| region.mesh.len())
            .sum::<usize>(),
        1536
    );
    Ok(())
}
