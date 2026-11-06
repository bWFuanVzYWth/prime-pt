//! 稀疏/兼容连续mesh借用与不复制几何的颜色属性候选。
use crate::mesh_parts::Fragment;
use crate::{FragmentMask, Mesh, Quad, TintPatch};
#[derive(Clone, Copy)]
enum Source<'a> {
    Dense(&'a [Mesh]),
    Sparse(&'a [Fragment]),
}
impl<'a> Source<'a> {
    fn revision(self, slot: usize) -> Option<u64> {
        match self {
            Self::Dense(meshes) => meshes.get(slot).map(|_| 0),
            Self::Sparse(fragments) => u16::try_from(slot)
                .ok()
                .and_then(|slot| {
                    fragments
                        .binary_search_by_key(&slot, |fragment| fragment.slot)
                        .ok()
                })
                .map(|index| fragments[index].revision),
        }
    }
    fn at(self, i: usize) -> Option<(usize, &'a Mesh)> {
        match self {
            Self::Dense(v) => v.get(i).map(|m| (i, m)),
            Self::Sparse(v) => v.get(i).map(|f| (usize::from(f.slot), &f.mesh)),
        }
    }
    fn get(self, slot: usize) -> Option<&'a Mesh> {
        match self {
            Self::Dense(v) => v.get(slot),
            Self::Sparse(v) => u16::try_from(slot)
                .ok()
                .and_then(|s| v.binary_search_by_key(&s, |f| f.slot).ok())
                .map(|i| &v[i].mesh),
        }
    }
}
/// 只读片段视图；生产路径的数量来自MeshParts缓存，构造不扫描所有片段。
#[derive(Clone, Copy)]
pub struct MeshView<'a> {
    base: Source<'a>,
    overlay: Option<Source<'a>>,
    changed: FragmentMask,
    len: usize,
    tints: &'a [TintPatch],
}
impl<'a> MeshView<'a> {
    /// 借用一个连续mesh。
    pub const fn from_mesh(mesh: &'a Mesh) -> Self {
        Self {
            base: Source::Dense(std::slice::from_ref(mesh)),
            overlay: None,
            changed: FragmentMask::empty(),
            len: mesh.quads.len(),
            tints: &[],
        }
    }
    /// 兼容64个section片段；兼容视图构造时累加数量。
    pub fn sections(base: &'a [Mesh; 64]) -> Self {
        Self::with_overlay(base, None)
    }
    /// 兼容64片段覆盖，空候选可以显式清除旧片段。
    pub fn with_overlay(base: &'a [Mesh; 64], overlay: Option<(&'a [Mesh; 64], u64)>) -> Self {
        let mut v = Self {
            base: Source::Dense(base),
            overlay: overlay.map(|(m, _)| Source::Dense(m)),
            changed: FragmentMask::from_words([overlay.map_or(0, |(_, m)| m), 0, 0, 0]),
            len: 0,
            tints: &[],
        };
        v.len = v.fragments().map(|(_, m)| m.quads.len()).sum();
        v
    }
    pub(crate) fn sparse(base: &'a [Fragment], len: usize) -> Self {
        Self {
            base: Source::Sparse(base),
            overlay: None,
            changed: FragmentMask::empty(),
            len,
            tints: &[],
        }
    }
    pub(crate) fn overlay_sparse(
        base: &'a [Fragment],
        next: &'a [Fragment],
        changed: FragmentMask,
        len: usize,
        tints: &'a [TintPatch],
    ) -> Self {
        Self {
            base: Source::Sparse(base),
            overlay: Some(Source::Sparse(next)),
            changed,
            len,
            tints,
        }
    }
    pub(crate) fn fragment(self, slot: usize) -> Option<&'a Mesh> {
        if self.changed.contains(slot) {
            self.overlay.and_then(|v| v.get(slot))
        } else {
            self.base.get(slot)
        }
    }
    /// 精确的源片段版本；只替换该片段才变化，颜色覆盖保留版本。
    ///
    /// 稀疏MeshParts由所有者发行单调版本；兼容连续mesh返回0，调用者须以其
    /// 整体geometry_revision作为保守回退。缺失片段返回None，不用地址或hash推测。
    pub fn fragment_revision(self, slot: usize) -> Option<u64> {
        if self.changed.contains(slot) {
            self.overlay.and_then(|source| source.revision(slot))
        } else {
            self.base.revision(slot)
        }
    }
    /// 实际quad量，O(1)。
    pub const fn len(self) -> usize {
        self.len
    }
    /// 是否没有几何。
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
    /// 只枚举实际非空片段，不分配。
    pub const fn fragments(self) -> Fragments<'a> {
        Fragments {
            view: self,
            base: 0,
            next: 0,
        }
    }
    /// 按primitive顺序借用几何；颜色候选由attributes消费。
    pub const fn quads(self) -> Quads<'a> {
        Quads {
            fragments: self.fragments(),
            mesh: None,
            slot: 0,
            quad: 0,
            remaining: self.len,
        }
    }
    /// 编码实际属性，包括未提交颜色候选。
    pub const fn attributes(self) -> Attributes<'a> {
        Attributes {
            quads: self.quads(),
            patches: self.tints,
            patch: 0,
        }
    }
    /// 是否仅附加颜色、没有替换几何片段。
    pub const fn attribute_only(self) -> bool {
        self.changed.is_empty() && !self.tints.is_empty()
    }
    /// 仅颜色变化范围；first相对region内primitive顺序。
    pub const fn attribute_ranges(self) -> AttributeRanges<'a> {
        AttributeRanges {
            fragments: self.fragments(),
            patches: self.tints,
            patch: 0,
            current: None,
            base: 0,
        }
    }
}
impl<'a> From<&'a Mesh> for MeshView<'a> {
    fn from(value: &'a Mesh) -> Self {
        Self::from_mesh(value)
    }
}
/// 两份有序分片目录的合并借用。
#[derive(Clone)]
pub struct Fragments<'a> {
    view: MeshView<'a>,
    base: usize,
    next: usize,
}
impl<'a> Iterator for Fragments<'a> {
    type Item = (usize, &'a Mesh);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let a = self.view.base.at(self.base);
            let b = self.view.overlay.and_then(|v| v.at(self.next));
            let slot = match (a, b) {
                (Some(a), Some(b)) => a.0.min(b.0),
                (Some(a), None) => a.0,
                (None, Some(b)) => b.0,
                (None, None) => return None,
            };
            if a.is_some_and(|a| a.0 == slot) {
                self.base += 1;
            }
            if b.is_some_and(|b| b.0 == slot) {
                self.next += 1;
            }
            let chosen = if self.view.changed.contains(slot) {
                b.filter(|b| b.0 == slot)
            } else {
                a.filter(|a| a.0 == slot)
            };
            if let Some((s, m)) = chosen
                && !m.quads.is_empty()
            {
                return Some((s, m));
            }
        }
    }
}
impl std::iter::FusedIterator for Fragments<'_> {}
/// 精确长度的几何迭代器；独占修改须等借用结束。
#[derive(Clone)]
pub struct Quads<'a> {
    fragments: Fragments<'a>,
    mesh: Option<&'a Mesh>,
    slot: usize,
    quad: usize,
    remaining: usize,
}
impl<'a> Quads<'a> {
    fn record(&mut self) -> Option<(usize, usize, &'a Quad)> {
        loop {
            if let Some(m) = self.mesh
                && let Some(q) = m.quads.get(self.quad)
            {
                let local = self.quad;
                self.quad += 1;
                self.remaining -= 1;
                return Some((self.slot, local, q));
            }
            let (s, m) = self.fragments.next()?;
            self.mesh = Some(m);
            self.slot = s;
            self.quad = 0;
        }
    }
}
impl<'a> Iterator for Quads<'a> {
    type Item = &'a Quad;
    fn next(&mut self) -> Option<Self::Item> {
        self.record().map(|(_, _, q)| q)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for Quads<'_> {}
impl std::iter::FusedIterator for Quads<'_> {}
/// 属性编码迭代器；只复制48B属性，不复制96B几何记录。
pub struct Attributes<'a> {
    quads: Quads<'a>,
    patches: &'a [TintPatch],
    patch: usize,
}
impl Iterator for Attributes<'_> {
    type Item = [[u32; 4]; 3];
    fn next(&mut self) -> Option<Self::Item> {
        let (s, i, q) = self.quads.record()?;
        while self
            .patches
            .get(self.patch)
            .is_some_and(|p| p.fragment() < s || p.fragment() == s && p.end() <= i)
        {
            self.patch += 1;
        }
        let mut v = q.attributes();
        if let Some(p) = self.patches.get(self.patch)
            && p.fragment() == s
            && p.first() <= i
            && i < p.end()
        {
            v[2][..3].copy_from_slice(&p.rgb().map(f32::to_bits));
        }
        Some(v)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.quads.size_hint()
    }
}
impl ExactSizeIterator for Attributes<'_> {}
impl std::iter::FusedIterator for Attributes<'_> {}
/// 已校验连续颜色范围；借用旧quad并在编码时覆盖乘色。
pub struct AttributeRange<'a> {
    /// region内首quad。
    pub first: usize,
    /// 连续旧quad借用。
    pub quads: &'a [Quad],
    /// 新ARGB乘色。
    pub argb: u32,
}
impl AttributeRange<'_> {
    /// 编码本范围属性，UV和材质身份保持原值。
    pub fn attributes(&self) -> impl ExactSizeIterator<Item = [[u32; 4]; 3]> + '_ {
        let rgb = [16, 8, 0].map(|s| ((self.argb >> s) & 255) as f32 / 255.);
        self.quads.iter().map(move |q| {
            let mut v = q.attributes();
            v[2][..3].copy_from_slice(&rgb.map(f32::to_bits));
            v
        })
    }
}
/// 单次合并片段目录与颜色范围，不遍历无关quad。
pub struct AttributeRanges<'a> {
    fragments: Fragments<'a>,
    patches: &'a [TintPatch],
    patch: usize,
    current: Option<(usize, &'a Mesh)>,
    base: usize,
}
impl<'a> Iterator for AttributeRanges<'a> {
    type Item = AttributeRange<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let p = *self.patches.get(self.patch)?;
        self.patch += 1;
        while self.current.is_none_or(|(s, _)| s < p.fragment()) {
            if let Some((_, m)) = self.current.take() {
                self.base += m.quads.len();
            }
            self.current = self.fragments.next();
            self.current?;
        }
        let (s, m) = self.current?;
        assert_eq!(s, p.fragment(), "准备时已验证颜色身份");
        Some(AttributeRange {
            first: self.base + p.first(),
            quads: &m.quads[p.first()..p.end()],
            argb: p.argb(),
        })
    }
}
impl std::iter::FusedIterator for AttributeRanges<'_> {}
