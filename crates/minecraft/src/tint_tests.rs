use super::*;
#[test]
fn native_and_custom_requests_keep_order_and_exact_integer_colors() {
    let resources = fixtures::resources().unwrap();
    let mut world = fixtures::world(&resources).unwrap();
    let requests = [
        TintRequest {
            position: [-17, -1, 15],
            state: 1,
            slot: 0,
        },
        TintRequest {
            position: [0, 64, 0],
            state: 4,
            slot: 0,
        },
        TintRequest {
            position: [0, 64, 0],
            state: 2,
            slot: -1,
        },
        TintRequest {
            position: [0, 64, 0],
            state: 3,
            slot: 0,
        },
        TintRequest {
            position: [0, 64, 0],
            state: 5,
            slot: 0,
        },
    ];
    for radius in 0..=7 {
        let plan = world.plan(&requests, &resources, radius).unwrap();
        assert_eq!(plan.host_requests(), &requests[1..2]);
        let ids = vec![0; plan.biome_sample_count()];
        let colors = world.finish(plan, &resources, &ids, &[0x12345678]).unwrap();
        assert_eq!(
            colors,
            [0xff336633, 0x12345678, 0xff224488, 0xffff3200, 0x80446688]
        );
    }
}
#[test]
fn source_cache_survives_radius_change_and_local_changes_recompute_only_dependencies() {
    let resources = fixtures::resources().unwrap();
    let mut world = fixtures::world(&resources).unwrap();
    let mut requests = fixtures::requests();
    let mut far = fixtures::requests();
    for r in &mut far {
        r.position[0] += 1600;
    }
    requests.extend(far);
    let plan = world.plan(&requests, &resources, 7).unwrap();
    let first = plan.biome_sample_count();
    let ids = vec![0; first];
    world.finish(plan, &resources, &ids, &[]).unwrap();
    let cached = world.plan(&requests, &resources, 7).unwrap();
    assert_eq!(cached.cached_colors(), 512);
    assert_eq!(cached.biome_sample_count(), 0);
    world.finish(cached, &resources, &[], &[]).unwrap();
    let smaller = world.plan(&requests, &resources, 0).unwrap();
    assert_eq!(smaller.cached_colors(), 0);
    assert_eq!(smaller.biome_sample_count(), 0);
    world.finish(smaller, &resources, &[], &[]).unwrap();
    world.invalidate(false, &HashSet::from([(0, 0)]));
    let plan = world.plan(&requests, &resources, 0).unwrap();
    assert!(plan.cached_colors() >= 256);
    assert!(plan.biome_sample_count() > 0);
    assert!(plan.biome_sample_count() < first);
    let ids = vec![1; plan.biome_sample_count()];
    let actual = world.finish(plan, &resources, &ids, &[]).unwrap();
    assert!(actual[..256].contains(&0xff99aa44));
    assert!(actual[256..].iter().all(|&c| c == 0xff336633));
    world.forget_section([0, 4, 0]);
    assert!(world.source.resident_keys().into_iter().all(|s| s.0 >= 2));
}
#[test]
fn invalid_response_does_not_populate_source_cache_and_stale_plan_is_rejected() {
    let resources = fixtures::resources().unwrap();
    let mut world = fixtures::world(&resources).unwrap();
    let requests = fixtures::requests();
    let plan = world.plan(&requests, &resources, 7).unwrap();
    let count = plan.biome_sample_count();
    assert!(
        world
            .finish(plan, &resources, &vec![2; count], &[])
            .is_err()
    );
    let plan = world.plan(&requests, &resources, 7).unwrap();
    assert_eq!(plan.biome_sample_count(), count);
    world.invalidate(true, &HashSet::new());
    assert!(
        world
            .finish(plan, &resources, &vec![0; count], &[])
            .is_err()
    );
    let plan = world.plan(&requests, &resources, 7).unwrap();
    assert_eq!(plan.biome_sample_count(), count);
}
#[test]
fn resource_identity_and_input_domains_are_checked_before_planning() {
    assert_eq!(std::mem::size_of::<BiomeRequest>(), 24);
    let resources = fixtures::resources().unwrap();
    let mut world = fixtures::world(&resources).unwrap();
    assert!(world.plan(&fixtures::requests(), &resources, 8).is_err());
    let unknown = [TintRequest {
        position: [0, 0, 0],
        state: 100,
        slot: 0,
    }];
    assert!(world.plan(&unknown, &resources, 0).is_err());
    let mut bytes = fixtures::world_bytes();
    bytes[8] = 2;
    assert!(TintWorld::decode(&bytes, &resources).is_err());
}

#[test]
fn plan_from_another_world_owner_is_rejected_even_with_equal_generation_and_serial() {
    let resources = fixtures::resources().unwrap();
    let requests = fixtures::requests();
    let mut a = fixtures::world(&resources).unwrap();
    let mut b = fixtures::world(&resources).unwrap();
    let a_plan = a.plan(&requests, &resources, 0).unwrap();
    let b_plan = b.plan(&requests, &resources, 0).unwrap();
    let count = b_plan.biome_sample_count();
    assert!(b.finish(a_plan, &resources, &vec![1; count], &[]).is_err());
    assert_eq!(
        b.finish(b_plan, &resources, &vec![0; count], &[]).unwrap(),
        vec![0xff336633; 256]
    );
}

#[test]
fn retiring_boundary_consumers_bounds_raw_zoom_and_blend_pages_during_long_movement() {
    let resources = fixtures::resources().unwrap();
    for radius in [0, 7] {
        let mut world = fixtures::world(&resources).unwrap();
        for sx in 0..64 {
            let mut requests = Vec::new();
            // 上下高度边界，且上半高草实际查询下方；源退休事件只有域内consumer key。
            for y in [-64, 319] {
                for z in [0, 15] {
                    for x in [0, 15] {
                        requests.push(TintRequest {
                            position: [sx * 16 + x, y, z],
                            state: 6,
                            slot: 0,
                        });
                    }
                }
            }
            let plan = world.plan(&requests, &resources, radius).unwrap();
            let ids = vec![0; plan.biome_sample_count()];
            world.finish(plan, &resources, &ids, &[]).unwrap();
            if sx > 0 {
                world.forget_section([sx - 1, -4, 0]);
                world.forget_section([sx - 1, 19, 0]);
            }
            let keys = world
                .blend
                .resident_keys()
                .into_iter()
                .chain(world.source.resident_keys());
            for Section(x, y, z) in keys {
                assert!((sx - 1..=sx + 1).contains(&x));
                assert!((-1..=1).contains(&z));
                assert!((-5..=-3).contains(&y) || (18..=20).contains(&y));
            }
        }
        world.forget_section([63, -4, 0]);
        world.forget_section([63, 19, 0]);
        assert!(world.blend.resident_keys().is_empty());
        assert!(world.source.resident_keys().is_empty());
    }
}

#[test]
fn a_different_resource_owner_cannot_reuse_world_cache_even_with_equal_generation() {
    let a = fixtures::resources().unwrap();
    let b = fixtures::resources().unwrap();
    let mut world = fixtures::world(&a).unwrap();
    let requests = fixtures::requests();
    assert!(world.plan(&requests, &b, 0).is_err());
    let plan = world.plan(&requests, &a, 0).unwrap();
    let ids = vec![0; plan.biome_sample_count()];
    assert!(world.finish(plan, &b, &ids, &[]).is_err());
    let retry = world.plan(&requests, &a, 0).unwrap();
    assert_eq!(retry.biome_sample_count(), ids.len());
}
