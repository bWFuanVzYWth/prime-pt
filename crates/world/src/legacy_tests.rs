use super::*;
use source::decode;

#[test]
fn world_camera_keeps_far_origin_fraction() {
    let source = WorldCamera {
        origin: [30_000_000.125, 34.5, -30_000_000.25],
        forward: [0., 0., -1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    };
    let anchor = source.anchor().unwrap();
    let relative = source.relative(anchor).unwrap();
    assert_eq!(relative.origin, [0.125, 34.5, 63.75, 0.]);
}

#[test]
fn scene_anchor_preserves_local_geometry_at_world_edge() {
    let mut near = Scene::new().unwrap();
    near.apply(&packet([0, 0, 0], true)).unwrap();
    let mut far = Scene::new().unwrap();
    far.apply(&packet([1_875_000, 0, 0], true)).unwrap();
    far.rebase([30_000_000., 0., 0.]);
    assert_eq!(
        far.merge().quads[0].positions(),
        near.merge().quads[0].positions()
    );
}

#[test]
fn arbitrary_quad_preserves_corner_bits_and_shared_attributes() {
    let positions = [
        [-0.0, 0.125, 3.0],
        [2.0, -0.25, 3.5],
        [1.25, 1.5, 2.75],
        [-0.5, 0.75, 4.0],
    ];
    let uv = [[-0.0, 0.25], [0.875, 0.5], [1.0, 0.75], [0.125, 1.0]];
    let tint = [0.2, -0.0, 1.0];
    let quad = Quad::new(positions, uv, tint, Material::Smooth);
    assert_eq!(
        quad.positions().map(|p| p.map(f32::to_bits)),
        positions.map(|p| p.map(f32::to_bits))
    );
    assert_eq!(
        quad.uv().map(|p| p.map(f32::to_bits)),
        uv.map(|p| p.map(f32::to_bits))
    );
    assert_eq!(quad.tint().map(f32::to_bits), tint.map(f32::to_bits));
    assert!(quad.valid());
}

fn packet(key: [i32; 3], quad: bool) -> Vec<u8> {
    let mut data = Vec::new();
    for word in [
        0x3154_5250,
        1,
        key[0] as u32,
        key[1] as u32,
        key[2] as u32,
        u32::from(quad),
    ] {
        data.extend(word.to_le_bytes());
    }
    if quad {
        for word in [
            0x321u32,
            1,
            0.25f32.to_bits(),
            0.0f32.to_bits(),
            (-0.5f32).to_bits(),
            0xff80_ff00,
        ] {
            data.extend(word.to_le_bytes());
        }
        for vertex in [
            [0.0f32, 0.0, 0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0, 1.0, 1.0],
            [0.0, 1.0, 0.0, 0.0, 1.0],
        ] {
            for value in vertex {
                data.extend(value.to_le_bytes());
            }
        }
    }
    data
}

#[test]
fn source_geometry_preserves_world_uv_and_tint() {
    let mut scene = Scene::new().unwrap();
    scene.apply(&packet([-1, 2, 3], true)).unwrap();
    let mesh = scene.merge();
    assert_eq!(mesh.quads[0].positions()[2], [-13.75, 35.0, 50.5]);
    assert_eq!(mesh.quads[0].uv()[2], [1.0, 1.0]);
    assert_eq!(mesh.quads[0].tint(), [128.0 / 255.0, 1.0, 0.0]);
    assert_eq!(mesh.quads.len(), 1);
}

#[test]
fn empty_section_replaces_existing_geometry() {
    let mut scene = Scene::new().unwrap();
    scene.apply(&packet([0, 0, 0], true)).unwrap();
    scene.apply(&packet([0, 0, 0], false)).unwrap();
    assert!(scene.merge().quads.is_empty());
}

#[test]
fn unchanged_source_preserves_geometry_revision() {
    let mut scene = Scene::new().unwrap();
    let bytes = packet([0, 0, 0], true);
    scene.apply(&bytes).unwrap();
    let revision = scene.entries().next().unwrap().1;
    scene.apply(&bytes).unwrap();
    assert_eq!(scene.entries().next().unwrap().1, revision);
}

#[test]
fn malformed_batch_keeps_previous_scene() {
    let mut scene = Scene::new().unwrap();
    scene.apply(&packet([0, 0, 0], true)).unwrap();
    let mut invalid = packet([0, 0, 0], true);
    invalid.pop();
    assert!(scene.apply(&invalid).is_err());
    assert_eq!(scene.merge().quads.len(), 1);
}

#[test]
fn negative_sections_use_euclidean_region() {
    let mut data = packet([-1, 0, 0], false);
    data[4..8].copy_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&packet([-4, 0, 0], false)[8..]);
    assert!(decode(&data).is_ok());
    let offset = 24;
    data[offset..offset + 4].copy_from_slice(&0i32.to_le_bytes());
    assert!(decode(&data).is_err());
}

#[test]
fn environment_requires_unit_sun_or_explicit_disable() {
    let disabled = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    assert!(!disabled.validated().unwrap().enabled());
    let invalid = Environment {
        sun_direction: [1., 1., 0.],
        ..Default::default()
    };
    assert!(invalid.validated().is_err());
}

#[test]
#[ignore = "先运行 Gradle wireFixture，生成真实 Java BakedQuad 源包"]
fn java_source_packet_preserves_geometry() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/java-source.prt");
    let bytes = std::fs::read(path).expect("请先运行 .\\gradlew.bat wireFixture");
    let mut scene = Scene::new().unwrap();
    scene.apply(&bytes).unwrap();
    let mesh = scene.merge();
    assert_eq!(mesh.quads.len(), 1);
    assert_eq!(mesh.quads[0].positions()[0], [-62.625, 2.5, 3.125]);
    assert_eq!(mesh.quads[0].uv()[0], [0.25, 0.5]);
    assert_eq!(
        mesh.quads[0].tint(),
        [128.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0]
    );
    assert_eq!(mesh.quads.len(), 1);
}

#[test]
#[ignore = "先运行Gradle wireFixture，生成生产Java writer的PRT2源包"]
fn java_ticketed_source_packet_preserves_wire_identity_and_geometry() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/java-source.prt2");
    let bytes = std::fs::read(path).expect("请先运行 .\\gradlew.bat wireFixture");
    let sources = decode(&bytes).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id, 0x0102_0304_0506_0708);
    assert_eq!(sources[0].key, [-4, 0, 0]);
    let mesh = compile(&sources[0]);
    assert_eq!(mesh.quads.len(), 1);
    assert_eq!(mesh.quads[0].positions()[0], [1.375, 2.5, 3.125]);
    assert_eq!(mesh.quads[0].uv()[0], [0.25, 0.5]);
    assert_eq!(
        mesh.quads[0].tint(),
        [128.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0]
    );
}
