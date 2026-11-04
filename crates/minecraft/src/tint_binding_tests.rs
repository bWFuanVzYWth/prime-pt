use crate::{
    Catalog, CompiledSlab, Neighbor, Neighborhood, SlabCompile, compile_slab, decode_sections,
    fixtures::{self, Pattern},
};

fn slab() -> (Catalog, CompiledSlab) {
    let catalog = fixtures::catalog().unwrap();
    let key = [3, -2, 5];
    let sources = decode_sections(&fixtures::section_packet(&[(1, key, Pattern::Tinted)])).unwrap();
    let mut sections = [Neighbor::Empty; 27];
    sections[13] = Neighbor::Available(sources[0].storage.as_ref().unwrap());
    let output = compile_slab(
        &catalog,
        &Neighborhood {
            key,
            generation: catalog.generation(),
            sections,
        },
        SlabCompile {
            first_y: 12,
            origin: [48., 32., 16.],
            max_quads: usize::MAX,
        },
    )
    .unwrap();
    (catalog, output)
}

#[test]
fn region_local_geometry_and_persistent_tint_refer_to_the_same_world_block() {
    let (catalog, output) = slab();
    assert!(!output.bindings.is_empty());
    assert!(output.bindings.len() < output.mesh.quads.len());
    for binding in &output.bindings {
        binding.validate(output.mesh.quads.len()).unwrap();
        let request = catalog.tint_request(1, [3, -2, 5], *binding).unwrap();
        assert_eq!((request.state, request.slot), (2, 0));
        for quad in
            &output.mesh.quads[binding.first as usize..(binding.first + binding.count) as usize]
        {
            for p in quad.positions() {
                let world = [p[0], p[1] - 64., p[2] + 64.];
                assert!(
                    (0..3).all(|axis| world[axis] >= request.position[axis] as f32
                        && world[axis] <= request.position[axis] as f32 + 1.)
                );
            }
        }
    }
}

#[test]
fn color_application_preserves_geometry_and_persistent_bindings() {
    let (_, mut output) = slab();
    let bindings = output.bindings.clone();
    let positions: Vec<_> = output
        .mesh
        .quads
        .iter()
        .map(|quad| quad.positions())
        .collect();
    output
        .apply_tints(&vec![0xff12ab34; bindings.len()])
        .unwrap();
    assert_eq!(output.bindings, bindings);
    assert_eq!(
        output
            .mesh
            .quads
            .iter()
            .map(|quad| quad.positions())
            .collect::<Vec<_>>(),
        positions
    );
    assert!(
        output
            .mesh
            .quads
            .iter()
            .all(|quad| quad.tint() == [18. / 255., 171. / 255., 52. / 255.])
    );
}

#[test]
fn invalid_binding_range_rejects_before_any_color_is_applied() {
    let (_, mut output) = slab();
    output.bindings.last_mut().unwrap().count = u32::MAX;
    let colors = vec![0xff000000; output.bindings.len()];
    assert!(output.apply_tints(&colors).is_err());
    assert!(output.mesh.quads.iter().all(|quad| quad.tint() == [1.; 3]));
}

#[test]
fn tint_sources_are_checked_against_the_explicit_resource_generation() {
    let (catalog, output) = slab();
    let binding = output.bindings[0];
    assert!(catalog.tint_request(2, [3, -2, 5], binding).is_err());
    assert!(
        catalog
            .tint_request(
                1,
                [3, -2, 5],
                crate::TintSpan {
                    source: u32::MAX,
                    ..binding
                }
            )
            .is_err()
    );
    assert!(
        catalog
            .tint_request(
                1,
                [3, -2, 5],
                crate::TintSpan {
                    local: 4096,
                    ..binding
                }
            )
            .is_err()
    );
}
