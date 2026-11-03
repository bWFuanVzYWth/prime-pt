//! 分片mesh借用；overlay只替换显式标记位置，不复制quad或延长所有权。
use crate::{Mesh, Quad};

/// 连续mesh或64个section分片的只读视图，按section顺序保持primitive身份。
#[derive(Clone, Copy)]
pub struct MeshView<'a> {
    base: &'a [Mesh],
    overlay: Option<(&'a [Mesh; 64], u64)>,
    len: usize,
}

impl<'a> MeshView<'a> {
    /// 借用一个连续mesh，供直接构造的渲染器输入使用。
    pub const fn from_mesh(mesh: &'a Mesh) -> Self {
        Self {
            base: std::slice::from_ref(mesh),
            overlay: None,
            len: mesh.quads.len(),
        }
    }

    /// 借用64个独占section；顺序是调用者约定的固定空间slot顺序。
    pub fn sections(base: &'a [Mesh; 64]) -> Self {
        Self::with_overlay(base, None)
    }

    /// 在同一slot次序内借用候选替换；mask位为1时使用overlay，空替换仍有效。
    ///
    /// 两份所有者须保持到迭代/渲染准备结束；本类型不提交替换或管理GPU完成。
    pub fn with_overlay(base: &'a [Mesh; 64], overlay: Option<(&'a [Mesh; 64], u64)>) -> Self {
        let mut view = Self {
            base,
            overlay,
            len: 0,
        };
        view.len = (0..64).map(|slot| view.fragment(slot).quads.len()).sum();
        view
    }

    fn fragment(self, slot: usize) -> &'a Mesh {
        if let Some((overlay, mask)) = self.overlay
            && mask & (1u64 << slot) != 0
        {
            &overlay[slot]
        } else {
            &self.base[slot]
        }
    }

    /// 返回实际可见quad数量，不包含被overlay替换的旧范围。
    pub const fn len(self) -> usize {
        self.len
    }
    /// 是否没有quad。
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
    /// 按固定section与源quad顺序遍历，不分配或复制顶点。
    pub const fn quads(self) -> Quads<'a> {
        Quads {
            view: self,
            slot: 0,
            quad: 0,
            remaining: self.len,
        }
    }
}

impl<'a> From<&'a Mesh> for MeshView<'a> {
    fn from(value: &'a Mesh) -> Self {
        Self::from_mesh(value)
    }
}

/// [`MeshView`]的精确长度迭代器；独占修改须等待此借用结束。
#[derive(Clone)]
pub struct Quads<'a> {
    view: MeshView<'a>,
    slot: usize,
    quad: usize,
    remaining: usize,
}
impl<'a> Iterator for Quads<'a> {
    type Item = &'a Quad;
    fn next(&mut self) -> Option<Self::Item> {
        while self.slot < self.view.base.len() {
            let mesh = self.view.fragment(self.slot);
            if let Some(quad) = mesh.quads.get(self.quad) {
                self.quad += 1;
                self.remaining -= 1;
                return Some(quad);
            }
            self.slot += 1;
            self.quad = 0;
        }
        None
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for Quads<'_> {}
impl std::iter::FusedIterator for Quads<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_replaces_empty_and_nonempty_slots_without_copying() {
        let mut base: [Mesh; 64] = std::array::from_fn(|_| Mesh::default());
        let mut overlay: [Mesh; 64] = std::array::from_fn(|_| Mesh::default());
        base[2].quads.push(Quad::default());
        base[4].quads.push(Quad::default());
        overlay[3].quads.push(Quad::default());
        let view = MeshView::with_overlay(&base, Some((&overlay, (1 << 2) | (1 << 3))));
        let quads: Vec<_> = view.quads().collect();
        assert_eq!(quads.len(), 2);
        assert!(std::ptr::eq(quads[0], &overlay[3].quads[0]));
        assert!(std::ptr::eq(quads[1], &base[4].quads[0]));
    }
}
