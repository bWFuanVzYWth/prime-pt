//! 稀疏分片、颜色候选与发布所有权的独立行为回归。
use super::*;
use crate::{MaterialId, Quad};

fn mesh(color: [f32; 3]) -> Mesh {
    Mesh {
        quads: vec![Quad::with_material(
            [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
            [[0.; 2]; 4],
            color,
            MaterialId::new(0),
        )],
    }
}

#[test]
fn publication_moves_unchanged_vectors_and_retains_only_old_dirty_payload() -> Result<(), Error> {
    let mut old = MeshParts::default();
    old.insert(3, mesh([1.; 3]))?;
    old.insert(200, mesh([0.5; 3]))?;
    let unchanged = old.get(200).unwrap().quads.as_ptr();
    let dirty = old.get(3).unwrap().quads.as_ptr();
    let mut next = MeshParts::default();
    next.insert(4, mesh([0.; 3]))?;
    let mut mask = FragmentMask::empty();
    mask.set(3)?;
    mask.set(4)?;
    let prepared = PreparedParts::prepare(&old, next, mask)?;
    assert_eq!(prepared.view(&old).quads().count(), 2);
    assert_eq!(old.len(), 2);
    let next = prepared.finish(&mut old);
    assert_eq!(next.len(), 2);
    assert!(next.get(3).is_none());
    assert_eq!(next.get(200).unwrap().quads.as_ptr(), unchanged);
    assert_eq!(old.len(), 1);
    assert_eq!(old.get(3).unwrap().quads.as_ptr(), dirty);
    assert!(old.get(200).is_none());
    Ok(())
}

#[test]
fn rejected_candidate_preserves_base_pointer_and_count() -> Result<(), Error> {
    let mut base = MeshParts::default();
    base.insert(255, mesh([1.; 3]))?;
    let pointer = base.get(255).unwrap().quads.as_ptr();
    let prepared = PreparedParts::prepare(&base, MeshParts::default(), FragmentMask::all())?;
    assert!(prepared.is_empty());
    drop(prepared);
    assert_eq!(base.len(), 1);
    assert_eq!(base.get(255).unwrap().quads.as_ptr(), pointer);
    Ok(())
}

#[test]
fn tint_overlay_is_visible_without_mutating_or_copying_base_geometry() -> Result<(), Error> {
    let mut base = MeshParts::default();
    base.insert(17, mesh([1.; 3]))?;
    base.insert(230, mesh([0.5; 3]))?;
    let pointer = base.get(230).unwrap().quads.as_ptr();
    let positions = base.get(230).unwrap().quads[0].positions();
    let candidate = PreparedParts::prepare(&base, MeshParts::default(), FragmentMask::empty())?
        .with_tints(&base, vec![TintPatch::new(230, 0, 1, 0xff12_3456)?])?;
    let view = candidate.view(&base);
    assert!(view.attribute_only());
    assert_eq!(view.quads().nth(1).unwrap().tint(), [0.5; 3]);
    let encoded: Vec<_> = view.attributes().collect();
    let ranges: Vec<_> = view.attribute_ranges().collect();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].first, 1);
    assert_eq!(ranges[0].attributes().next(), Some(encoded[1]));
    assert_eq!(
        encoded[1][2][..3],
        [18. / 255., 52. / 255., 86. / 255.].map(f32::to_bits)
    );
    let published = candidate.finish(&mut base);
    assert_eq!(published.get(230).unwrap().quads.as_ptr(), pointer);
    assert_eq!(published.get(230).unwrap().quads[0].positions(), positions);
    assert_eq!(
        published.get(230).unwrap().quads[0].tint(),
        [18. / 255., 52. / 255., 86. / 255.]
    );
    assert_eq!(base.len(), 0);
    Ok(())
}

#[test]
fn overlapping_or_out_of_range_tint_candidates_are_rejected() -> Result<(), Error> {
    let mut base = MeshParts::default();
    base.insert(7, mesh([1.; 3]))?;
    let candidate = PreparedParts::prepare(&base, MeshParts::default(), FragmentMask::empty())?;
    assert!(
        candidate
            .with_tints(&base, vec![TintPatch::new(7, 0, 2, 0)?])
            .is_err()
    );
    let candidate = PreparedParts::prepare(&base, MeshParts::default(), FragmentMask::empty())?;
    assert!(
        candidate
            .with_tints(
                &base,
                vec![TintPatch::new(7, 0, 1, 0)?, TintPatch::new(7, 0, 1, 1)?]
            )
            .is_err()
    );
    assert_eq!(base.get(7).unwrap().quads[0].tint(), [1.; 3]);
    Ok(())
}

#[test]
fn sorted_tint_patches_keep_global_offsets_across_sparse_fragments() -> Result<(), Error> {
    let mut base = MeshParts::default();
    for (slot, count) in [(1, 3), (79, 2), (255, 4)] {
        base.insert(
            slot,
            Mesh {
                quads: vec![mesh([1.; 3]).quads[0]; count],
            },
        )?;
    }
    let candidate = PreparedParts::prepare(&base, MeshParts::default(), FragmentMask::empty())?
        .with_tints(
            &base,
            vec![
                TintPatch::new(255, 3, 1, 0xff00_ff00)?,
                TintPatch::new(1, 2, 1, 0xff00_00ff)?,
                TintPatch::new(79, 0, 2, 0xffff_0000)?,
            ],
        )?;
    let full: Vec<_> = candidate.view(&base).attributes().collect();
    let ranges: Vec<_> = candidate.view(&base).attribute_ranges().collect();
    assert_eq!(
        ranges.iter().map(|r| r.first).collect::<Vec<_>>(),
        [2, 3, 8]
    );
    for range in ranges {
        assert_eq!(
            range.attributes().collect::<Vec<_>>(),
            full[range.first..range.first + range.quads.len()]
        );
    }
    let published = candidate.finish(&mut base);
    assert_eq!(published.view().attributes().collect::<Vec<_>>(), full);
    Ok(())
}

#[test]
fn sparse_fragment_order_matches_legacy_dense_overlay() -> Result<(), Error> {
    let mut dense: [Mesh; 64] = std::array::from_fn(|_| Mesh::default());
    let mut overlay: [Mesh; 64] = std::array::from_fn(|_| Mesh::default());
    dense[2] = mesh([1.; 3]);
    dense[4] = mesh([0.5; 3]);
    overlay[3] = mesh([0.; 3]);
    let view = crate::MeshView::with_overlay(&dense, Some((&overlay, (1 << 2) | (1 << 3))));
    assert_eq!(view.len(), 2);
    assert_eq!(view.quads().count(), 2);
    assert!(std::ptr::eq(
        view.quads().next().unwrap(),
        &overlay[3].quads[0]
    ));
    Ok(())
}
