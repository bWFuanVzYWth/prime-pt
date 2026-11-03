//! State资源和候选的分片所有权；准备期间只借旧分片，提交时再转移。
use prime_scene::{Mesh, MeshView};

pub(crate) static EMPTY: [Mesh; 64] = [const { Mesh { quads: Vec::new() } }; 64];

pub(crate) struct RegionMesh {
    pub parts: Option<Box<[Mesh; 64]>>,
    pub changed: u64,
    pub revision: u64,
    pub content_revision: u64,
    pub available: bool,
    pub excluded: bool,
}

impl RegionMesh {
    pub fn parts(&self) -> &[Mesh; 64] {
        self.parts.as_deref().unwrap_or(&EMPTY)
    }
    pub fn owned_quads(&self) -> usize {
        self.parts().iter().map(|mesh| mesh.quads.len()).sum()
    }
    pub fn view(&self) -> MeshView<'_> {
        MeshView::sections(self.parts())
    }
    pub fn over<'a>(&'a self, old: &'a Self) -> MeshView<'a> {
        MeshView::with_overlay(old.parts(), Some((self.parts(), self.changed)))
    }
    pub fn same_output(&self, old: &Self) -> (bool, bool) {
        let same_partition = (0..64)
            .filter(|slot| self.changed & (1 << slot) != 0)
            .all(|slot| self.parts()[slot].quads.len() == old.parts()[slot].quads.len());
        if same_partition {
            let mut content = true;
            for slot in (0..64).filter(|slot| self.changed & (1 << slot) != 0) {
                for (a, b) in self.parts()[slot]
                    .quads
                    .iter()
                    .zip(&old.parts()[slot].quads)
                {
                    if !a.same_geometry(b) {
                        return (false, false);
                    }
                    content &= a.same_attributes(b);
                }
            }
            (true, content)
        } else {
            // 分片边界可变化而扁平primitive顺序恰好不变，保留精确回退。
            let before = old.view();
            let next = self.over(old);
            if before.len() != next.len() {
                return (false, false);
            }
            before
                .quads()
                .zip(next.quads())
                .fold((true, true), |(g, c), (a, b)| {
                    (
                        g && a.same_geometry(b),
                        c && a.same_geometry(b) && a.same_attributes(b),
                    )
                })
        }
    }
    /// 仅在GPU准备已成功、旧消费者已结束的publish_with回调中调用。
    /// 遍历固定数组、move Vec，无分配或其他可失败操作。
    pub fn inherit(old: &mut Self, next: &mut Self) {
        if let (Some(old), Some(parts)) = (&mut old.parts, &mut next.parts) {
            for slot in 0..64 {
                if next.changed & (1 << slot) == 0 {
                    parts[slot] = std::mem::take(&mut old[slot]);
                }
            }
        }
        next.changed = u64::MAX;
    }
}
