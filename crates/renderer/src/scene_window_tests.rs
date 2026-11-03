//! 完整 LOD0 ticket 路径的 GPU 窗口消费者，不使用 PRT1 fixture 旁路。

use super::*;

fn blases(gpu: &Gpu) -> std::collections::BTreeMap<[i32; 3], Option<vulkan::TriangleBlas>> {
    gpu.region_cache
        .entries()
        .iter()
        .map(|entry| (entry.key, entry.blas))
        .collect()
}

#[test]
#[ignore = "真实Vulkan RT；移动加载保留旧覆盖且重叠region复用BLAS，串行执行"]
fn hardware_pt_lod0_window_keeps_overlap_through_movement_and_return() -> Result {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = prime_world::Scene::new()?;
    let camera = Camera {
        origin: [8., 8., -12., 0.],
        forward: [0., 0., 1., 0.],
        right: [0.8, 0., 0., 0.],
        up: [0., 0.8, 0., 0.],
    };
    let original = world_camera(camera);
    let keys = [[-1, 0, 0], [0, 0, 0], [8, 0, 0]];
    let input = sources(&keys, false, 0x00bf_d9a6, false);
    settle(&mut scene, &mut gpu, &original, &input)?;
    let initial = blases(&gpu);
    assert_eq!(
        initial.keys().copied().collect::<Vec<_>>(),
        [[-1, 0, 0], [0, 0, 0]]
    );
    let image = image(&mut gpu)?;
    let before = host.render(&mut gpu, &image, &original.relative(scene.anchor())?)?;

    let moved = crate::scene::WorldCamera {
        origin: [72., 8., -12.],
        ..original
    };
    let mut requests = Vec::new();
    for _ in 0..4_096 {
        requests = scene
            .plan(&moved, VIEW_DISTANCE, MIN_SECTION_Y, MAX_SECTION_Y, 64)?
            .to_vec();
        if scene.prepare()? {
            if let Err(error) = gpu.set_sections(&scene) {
                scene.reject()?;
                return Err(error.into());
            }
            scene.commit()?;
        }
        assert_eq!(blases(&gpu), initial, "新窗口未完成时保留全部旧GPU覆盖");
        if !requests.is_empty() {
            break;
        }
    }
    assert!(!requests.is_empty(), "移动窗口必须最终派发真实前沿请求");
    scene.apply(&packet(&requests, &input))?;
    settle(&mut scene, &mut gpu, &moved, &input)?;
    let shifted = blases(&gpu);
    assert_eq!(
        shifted.keys().copied().collect::<Vec<_>>(),
        [[0, 0, 0], [2, 0, 0]]
    );
    assert_eq!(
        shifted[&[0, 0, 0]],
        initial[&[0, 0, 0]],
        "重叠region的BLAS必须保留"
    );

    // 回转重新捕获已离开的后沿，交集仍使用原BLAS，probe相机始终观察相同绝对位置。
    settle(&mut scene, &mut gpu, &original, &input)?;
    let returned = blases(&gpu);
    assert_eq!(
        returned.keys().copied().collect::<Vec<_>>(),
        [[-1, 0, 0], [0, 0, 0]]
    );
    assert_eq!(returned[&[0, 0, 0]], initial[&[0, 0, 0]]);
    let after = host.render(&mut gpu, &image, &original.relative(scene.anchor())?)?;
    assert!(
        before.iter().zip(&after).all(|(&a, &b)| a.abs_diff(b) <= 1),
        "回转后的真实宿主图像未恢复相同世界覆盖"
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
