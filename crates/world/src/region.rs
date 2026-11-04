//! State资源独占稀疏slab与持久tint绑定；GPU成功后才移动旧片段和改写颜色。
use crate::Error;
use prime_minecraft::TintSpan;
use prime_scene::{FragmentMask, MeshParts, MeshView, PreparedParts, TintPatch};

pub(crate) struct FragmentBindings {
    pub fragment: usize,
    pub spans: Vec<TintSpan>,
    pub colors: Vec<u32>,
    pub custom: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct BindingColor {
    pub fragment: usize,
    pub span: usize,
    pub argb: u32,
}

pub(crate) struct RegionMesh {
    pub key: [i32; 3],
    pub parts: MeshParts,
    prepared: Option<PreparedParts>,
    pub changed: FragmentMask,
    pub bindings: Vec<FragmentBindings>,
    binding_merge: Vec<FragmentBindings>,
    color_updates: Vec<BindingColor>,
    comparison: (bool, bool),
    pub revision: u64,
    pub content_revision: u64,
    pub missing_sections: u64,
    pub excluded: bool,
}

pub(crate) struct PartialRegion {
    pub parts: MeshParts,
    pub changed: FragmentMask,
    pub bindings: Vec<FragmentBindings>,
    pub color_updates: Vec<BindingColor>,
    pub missing_sections: u64,
}

impl RegionMesh {
    #[allow(clippy::too_many_arguments)]
    pub fn candidate(
        key: [i32; 3],
        old: Option<&Self>,
        next: MeshParts,
        changed: FragmentMask,
        mut bindings: Vec<FragmentBindings>,
        mut patches: Vec<TintPatch>,
        color_updates: Vec<BindingColor>,
        revision: u64,
        content_revision: u64,
        missing_sections: u64,
        excluded: bool,
    ) -> Result<Self, Error> {
        bindings.sort_unstable_by_key(|entry| entry.fragment);
        patches.sort_unstable_by_key(|patch| (patch.fragment(), patch.first()));
        let mut binding_merge = Vec::new();
        if let Some(old) = old {
            let comparison = compare_changed(
                &old.parts,
                &next,
                changed,
                &patches,
                color_updates.is_empty(),
            );
            binding_merge.try_reserve_exact(old.bindings.len() + bindings.len())?;
            let prepared = PreparedParts::prepare(&old.parts, next, changed)?
                .with_tints(&old.parts, patches)?;
            Ok(Self {
                key,
                parts: MeshParts::default(),
                prepared: Some(prepared),
                changed,
                bindings,
                binding_merge,
                color_updates,
                comparison,
                revision,
                content_revision,
                missing_sections,
                excluded,
            })
        } else {
            let mut empty = MeshParts::default();
            let parts = PreparedParts::prepare(&empty, next, changed)?
                .with_tints(&empty, patches)?
                .finish(&mut empty);
            Ok(Self {
                key,
                parts,
                prepared: None,
                changed,
                bindings,
                binding_merge,
                color_updates,
                comparison: (false, false),
                revision,
                content_revision,
                missing_sections,
                excluded,
            })
        }
    }

    pub fn len(&self) -> usize {
        self.prepared
            .as_ref()
            .map_or(self.parts.len(), PreparedParts::len)
    }

    pub fn owned_quads(&self) -> usize {
        self.prepared
            .as_ref()
            .map_or(self.parts.len(), PreparedParts::owned_len)
    }

    pub fn fragment_quads(&self, mask: FragmentMask) -> usize {
        mask.iter()
            .filter_map(|slot| self.parts.get(slot))
            .map(|mesh| mesh.quads.len())
            .sum()
    }

    pub fn into_partial(self) -> PartialRegion {
        PartialRegion {
            parts: self.prepared.map_or(self.parts, PreparedParts::into_owned),
            changed: self.changed,
            bindings: self.bindings,
            color_updates: self.color_updates,
            missing_sections: self.missing_sections,
        }
    }

    pub fn view(&self) -> MeshView<'_> {
        assert!(self.prepared.is_none(), "候选借用必须指定旧所有者");
        self.parts.view()
    }

    pub fn over<'a>(&'a self, old: &'a Self) -> MeshView<'a> {
        self.prepared
            .as_ref()
            .map_or_else(|| self.parts.view(), |next| next.view(&old.parts))
    }

    pub const fn same_output(&self, _old: &Self) -> (bool, bool) {
        self.comparison
    }

    /// 已预留的目录合并；旧dirty几何留旧资源，未变几何/绑定整体移动。
    pub fn inherit(old: &mut Self, next: &mut Self) {
        if let Some(prepared) = next.prepared.take() {
            next.parts = prepared.finish(&mut old.parts);
            let mut carried = old
                .bindings
                .extract_if(.., |entry| !next.changed.contains(entry.fragment))
                .peekable();
            let mut built = next.bindings.drain(..).peekable();
            while carried.peek().is_some() || built.peek().is_some() {
                let from_old = match (carried.peek(), built.peek()) {
                    (Some(a), Some(b)) => a.fragment < b.fragment,
                    (Some(_), None) => true,
                    _ => false,
                };
                next.binding_merge.push(if from_old {
                    carried.next().expect("已检查旧绑定")
                } else {
                    built.next().expect("已检查新绑定")
                });
            }
            drop(carried);
            drop(built);
            std::mem::swap(&mut next.bindings, &mut next.binding_merge);
            for update in next.color_updates.drain(..) {
                let index = next
                    .bindings
                    .binary_search_by_key(&update.fragment, |entry| entry.fragment)
                    .expect("准备时验证的绑定片段");
                next.bindings[index].colors[update.span] = update.argb;
            }
        }
        next.changed = FragmentMask::all();
    }
}

/// 未变片段由所有权继承保证相同；只比较dirty输出与其颜色范围，不重读整个region。
fn compare_changed(
    old: &MeshParts,
    next: &MeshParts,
    changed: FragmentMask,
    patches: &[TintPatch],
    mut content: bool,
) -> (bool, bool) {
    let mut at = 0;
    for fragment in changed.iter() {
        let before = old
            .get(fragment)
            .map_or(&[][..], |mesh| mesh.quads.as_slice());
        let after = next
            .get(fragment)
            .map_or(&[][..], |mesh| mesh.quads.as_slice());
        if before.len() != after.len() {
            return (false, false);
        }
        while at < patches.len() && patches[at].fragment() < fragment {
            at += 1;
        }
        for (index, (before, after)) in before.iter().zip(after).enumerate() {
            if !before.same_geometry(after) {
                return (false, false);
            }
            if content {
                while at < patches.len()
                    && patches[at].fragment() == fragment
                    && patches[at].first() + patches[at].count() <= index
                {
                    at += 1;
                }
                let mut attributes = after.attributes();
                if let Some(patch) = patches
                    .get(at)
                    .filter(|patch| patch.fragment() == fragment && patch.first() <= index)
                {
                    attributes[2][..3]
                        .copy_from_slice(&[16, 8, 0].map(|shift| {
                            (((patch.argb() >> shift) & 255) as f32 / 255.).to_bits()
                        }));
                }
                content = before.attributes() == attributes;
            }
        }
    }
    (true, content)
}
