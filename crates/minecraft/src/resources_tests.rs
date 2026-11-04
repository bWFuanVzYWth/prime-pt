//! 资源世代与动画源布局的输入边界行为。
use crate::{Catalog, Error, fixtures};

#[test]
fn air_proof_uses_actual_uniform_state_and_rejects_unknown_identity() {
    let mut catalog = fixtures::catalog().unwrap();
    let air = crate::SectionStorage::new(0, vec![0], vec![]).unwrap();
    let invisible_fluid = crate::SectionStorage::new(0, vec![3], vec![]).unwrap();
    assert!(catalog.section_is_air(&air).unwrap());
    assert!(!catalog.section_is_air(&invisible_fluid).unwrap());
    let nonuniform = crate::SectionStorage::new(1, vec![0, 1], vec![0; 64]).unwrap();
    assert!(!catalog.section_is_air(&nonuniform).unwrap());
    assert!(
        catalog
            .section_is_air(&crate::SectionStorage::new(0, vec![u32::MAX], vec![]).unwrap())
            .is_err()
    );
    catalog.states.get_mut(&0).unwrap().flags = 16;
    assert!(!catalog.section_is_air(&air).unwrap());
}

#[test]
fn failed_or_abandoned_resource_candidate_preserves_the_published_catalog() {
    let mut catalog = fixtures::catalog().unwrap();
    let mut bytes = fixtures::resource_packet_generation(2);
    {
        let mut prepared = catalog.stage_resources(&bytes).unwrap();
        let maps = prepared
            .material_resources([32, 16], &[160, 200, 100, 255].repeat(32 * 16))
            .unwrap();
        assert!(!maps.materials.is_empty());
    }
    assert_eq!(catalog.generation(), 1);
    bytes.truncate(41);
    assert!(catalog.stage_resources(&bytes).is_err());
    assert_eq!(catalog.generation(), 1);
    assert!(
        catalog
            .stage_resources(&fixtures::resource_packet())
            .is_err()
    );
}

fn animated_sprite(frame: u32, complete_pixels: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let word = |bytes: &mut Vec<u8>, value: u32| bytes.extend(value.to_le_bytes());
    for value in [0x314d5250, 263, 1, 0, 1, 0, 0, 0, 0, 1, 1, 1] {
        word(&mut bytes, value);
    }
    bytes.push(b'a');
    for value in [0f32, 0., 1., 1.] {
        word(&mut bytes, value.to_bits());
    }
    for value in [1, 1, 2, 2, 0, 1, frame, 3, 1, 1, 2] {
        word(&mut bytes, value);
    }
    word(&mut bytes, if complete_pixels { 8 } else { 0 });
    if complete_pixels {
        bytes.extend([255, 0, 0, 255, 0, 255, 0, 0]);
    }
    word(&mut bytes, 0);
    word(&mut bytes, 0);
    bytes
}

#[test]
fn animation_source_keeps_declared_order_duration_and_full_rgba_sheet() {
    let mut catalog = Catalog::default();
    let prepared = catalog.stage_resources(&animated_sprite(1, true)).unwrap();
    let sprite = prepared.sprites().next().unwrap();
    assert_eq!(sprite.frames, [[0, 1], [1, 3]]);
    assert!(sprite.interpolate);
    assert_eq!(sprite.images[0].pixels, [255, 0, 0, 255, 0, 255, 0, 0]);
}

#[test]
fn animation_requires_pixels_and_indices_inside_the_declared_sheet() {
    let mut catalog = Catalog::default();
    for bytes in [animated_sprite(2, true), animated_sprite(1, false)] {
        assert!(matches!(
            catalog.stage_resources(&bytes),
            Err(Error::Input("sprite动画帧与源图像布局不一致"))
        ));
    }
}

#[test]
#[ignore = "先执行Gradle compactSourceFixture；离屏实际26.3 palette/storage"]
fn actual_java_palette_writer_matches_all_source_cells() {
    let path = std::env::var_os("PRIME_COMPACT_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/java-compact.bin")
        });
    let bytes = std::fs::read(path).unwrap();
    let mut reader = crate::wire::Reader::new(&bytes).unwrap();
    assert_eq!(reader.u32().unwrap(), 0x31464350);
    assert_eq!(reader.u32().unwrap(), 263);
    for pattern in 0..reader.u32().unwrap() {
        let count = reader.count(1).unwrap();
        let mut decoded = crate::decode_sections(reader.take(count).unwrap()).unwrap();
        assert_eq!(decoded.len(), 1);
        let section = decoded.pop().unwrap();
        assert_eq!(section.request, u64::from(pattern) + 1);
        assert_eq!(section.key, [-2, pattern as i32 - 1, 4]);
        let storage = section.storage.unwrap();
        for index in 0..4096 {
            assert_eq!(
                storage.state(index),
                Some(reader.u32().unwrap()),
                "pattern {pattern}, index {index}"
            );
        }
    }
    reader.finish().unwrap();
}
