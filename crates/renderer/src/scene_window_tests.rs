//! 完整 LOD0 ticket 路径的 GPU 窗口消费者，不使用 PRT1 fixture 旁路。

use super::*;

#[test]
#[ignore = "真实 Vulkan；宿主每帧重新 plan，未传完的 64³ 区域须最终共同发布"]
fn hardware_pt_manager_publishes_a_region_over_successive_frame_budgets() -> Result {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.set_atlas(16, 16, &atlas())?;
    gpu.set_terrain_upload_budget(1024)?;
    let mut scene = prime_world::Scene::new()?;
    let camera = world_camera(Camera {
        origin: [8., 8., -12., 0.],
        forward: [0., 0., 1., 0.],
        right: [0.8, 0., 0., 0.],
        up: [0., 0.8, 0., 0.],
    });
    let input = sources(&[[0; 3]], false, 0x00bf_d9a6, false);
    scene.rebase(camera.anchor()?);
    let mut waited = false;
    for _ in 0..2048 {
        let requests = scene
            .plan(&camera, VIEW_DISTANCE, MIN_SECTION_Y, MAX_SECTION_Y, 64)?
            .to_vec();
        if !requests.is_empty() {
            scene.apply(&packet(&requests, &input))?;
        }
        if scene.prepare()? || gpu.scene_resources_dirty() {
            let regions: Vec<_> = scene.render_regions().collect();
            let ready = gpu.stream_regions(prime_scene::RenderView {
                generation: scene.generation(),
                anchor: scene.anchor(),
                regions: &regions,
            })?;
            assert!(gpu.scene_timing().unwrap().terrain_upload_bytes <= 1024);
            if ready {
                scene.commit()?;
            } else {
                waited = true;
                scene.park_publication();
            }
        }
        if !scene.pending() && !gpu.terrain_uploads.pending() {
            break;
        }
    }
    assert!(waited && !scene.pending() && !gpu.terrain_uploads.pending());
    assert_eq!(scene.entries().len(), 1);
    assert_eq!(gpu.region_cache.entries().len(), 1);
    let image = image(&mut gpu)?;
    host.render(&mut gpu, &image, &camera.relative(scene.anchor())?)?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
use simple_engine::vulkan;

#[test]
#[ignore = "真实 Vulkan；分帧上传途中撤销编辑，继续使用完整原场景"]
fn hardware_pt_superseded_upload_keeps_published_geometry() -> Result {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = prime_world::Scene::new()?;
    let camera = world_camera(Camera {
        origin: [24., 24., -12., 0.],
        ..super::super::camera()
    });
    let original = sources(&[EDIT_KEY], false, 0x00bf_d9a6, false);
    let edited = sources(&[EDIT_KEY], true, 0x00bf_d9a6, false);
    settle(&mut scene, gpu, &camera, &original)?;
    let top = gpu.scene.top;
    gpu.set_terrain_upload_budget(1024)?;
    scene.invalidate_section(EDIT_KEY)?;
    let mut superseded = false;
    let start = std::time::Instant::now();
    loop {
        let requests = scene.plan(&camera, VIEW_DISTANCE, MIN_SECTION_Y, MAX_SECTION_Y, 64)?;
        if !requests.is_empty() {
            let packet = packet(requests, if superseded { &original } else { &edited });
            scene.apply(&packet)?;
        }
        if scene.prepare()? || gpu.terrain_pending() {
            let regions: Vec<_> = scene.render_regions().collect();
            if gpu.stream_regions(prime_scene::RenderView {
                generation: scene.generation(),
                anchor: scene.anchor(),
                regions: &regions,
            })? {
                scene.commit()?;
            } else {
                scene.park_publication();
                if !superseded && gpu.scene_timing().unwrap().terrain_upload_bytes != 0 {
                    scene.invalidate_section(EDIT_KEY)?;
                    superseded = true;
                }
            }
        }
        assert_eq!(gpu.scene.top, top, "失效版本不得替换已发布的 TLAS");
        if superseded && !scene.pending() && !gpu.terrain_pending() {
            break;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(15));
        std::thread::yield_now();
    }
    let output = image(gpu)?;
    let camera = camera.relative(gpu.scene_anchor())?;
    host.render(gpu, &output, &camera)?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

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
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
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
    // 两次像素比较共用射线样本与 jitter 起点。场景更新会保留绝对随机序号。
    gpu.test_restart_sample_sequence(0)?;
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
        let current = blases(&gpu);
        assert_eq!(
            current[&[0, 0, 0]],
            initial[&[0, 0, 0]],
            "需求交集始终保留旧GPU覆盖"
        );
        assert!(
            current.keys().all(|key| initial.contains_key(key)),
            "未回复源不能产生新几何"
        );
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
    gpu.test_restart_sample_sequence(0)?;
    let after = host.render(&mut gpu, &image, &original.relative(scene.anchor())?)?;
    assert!(
        before.iter().zip(&after).all(|(&a, &b)| a.abs_diff(b) <= 1),
        "回转后的真实宿主图像未恢复相同世界覆盖"
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn refresh_packet(
    requests: &[prime_world::CaptureRequest],
    input: &Sources,
    unavailable: Option<[i32; 3]>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    word(&mut bytes, 0x3254_5250);
    word(&mut bytes, requests.len() as u32);
    for request in requests {
        if unavailable == Some(request.section) {
            bytes.extend_from_slice(&request.id.to_le_bytes());
            for axis in request.section {
                word(&mut bytes, axis as u32);
            }
            word(&mut bytes, u32::MAX);
        } else {
            bytes.extend_from_slice(&packet(std::slice::from_ref(request), input)[8..]);
        }
    }
    bytes
}

fn publish_step(scene: &mut prime_world::Scene, gpu: &mut Gpu) -> Result {
    if scene.prepare()? {
        if let Err(error) = gpu.set_sections(scene) {
            scene.reject()?;
            return Err(error.into());
        }
        scene.commit()?;
    }
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；刷新缺源保留旧BLAS，候选齐备且GPU成功后无空帧替换"]
fn hardware_pt_region_refresh_never_drops_published_cover_for_unavailable_source() -> Result {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = prime_world::Scene::new()?;
    let camera = world_camera(Camera {
        origin: [24., 24., -12., 0.],
        ..super::super::camera()
    });
    let input = sources(&[EDIT_KEY], false, 0x00bf_d9a6, false);
    settle(&mut scene, &mut gpu, &camera, &input)?;
    let previous = blases(&gpu);
    let top = gpu.scene.top;
    let attributes = gpu.scene.quads;
    assert_eq!(previous.keys().copied().collect::<Vec<_>>(), [[0, 0, 0]]);

    scene.invalidate_section(EDIT_KEY)?;
    let mut unavailable_seen = false;
    for _ in 0..4096 {
        let requests = scene
            .plan(&camera, VIEW_DISTANCE, MIN_SECTION_Y, MAX_SECTION_Y, 64)?
            .to_vec();
        unavailable_seen |= requests.iter().any(|request| request.section == EDIT_KEY);
        if !requests.is_empty() {
            scene.apply(&refresh_packet(&requests, &input, Some(EDIT_KEY)))?;
        }
        publish_step(&mut scene, &mut gpu)?;
        assert_eq!(blases(&gpu), previous, "缺源与负缓存不得撤下已发布几何");
        assert_eq!(gpu.scene.top, top);
        assert_eq!(gpu.scene.quads, attributes);
        if !scene.pending() {
            break;
        }
    }
    assert!(unavailable_seen && !scene.pending());

    scene.invalidate_section(EDIT_KEY)?;
    let edited = sources(&[EDIT_KEY], true, 0x00bf_d9a6, false);
    let mut held = None;
    for _ in 0..4096 {
        let requests = scene
            .plan(&camera, VIEW_DISTANCE, MIN_SECTION_Y, MAX_SECTION_Y, 64)?
            .to_vec();
        held = requests
            .iter()
            .copied()
            .find(|request| request.section == EDIT_KEY);
        let other: Vec<_> = requests
            .into_iter()
            .filter(|request| request.section != EDIT_KEY)
            .collect();
        if !other.is_empty() {
            scene.apply(&packet(&other, &edited))?;
        }
        publish_step(&mut scene, &mut gpu)?;
        assert_eq!(blases(&gpu), previous, "新源未回复时仍显示旧BLAS");
        if held.is_some() {
            break;
        }
    }
    scene.apply(&packet(&[held.ok_or("刷新未请求缺失源")?], &edited))?;
    for _ in 0..4096 {
        let requests = scene
            .plan(&camera, VIEW_DISTANCE, MIN_SECTION_Y, MAX_SECTION_Y, 64)?
            .to_vec();
        if !requests.is_empty() {
            scene.apply(&packet(&requests, &edited))?;
        }
        publish_step(&mut scene, &mut gpu)?;
        assert_eq!(
            blases(&gpu).keys().copied().collect::<Vec<_>>(),
            [[0, 0, 0]],
            "每次提交均有完整覆盖"
        );
        if !scene.pending() {
            break;
        }
    }
    assert!(!scene.pending());
    assert_ne!(
        blases(&gpu)[&[0, 0, 0]],
        previous[&[0, 0, 0]],
        "完整新几何上传后才替换BLAS"
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
