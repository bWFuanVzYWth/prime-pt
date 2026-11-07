//! 稀疏分片独占所有者；候选先预留合并目录，发布阶段只移动已有分配。
use crate::{Error, Mesh, MeshView};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// 64个section各四个slab的变化位；显式空替换也保留置位。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FragmentMask([u64; 4]);
impl FragmentMask {
    /// 没有变化片段。
    pub const fn empty() -> Self {
        Self([0; 4])
    }
    /// 全部256片段。
    pub const fn all() -> Self {
        Self([u64::MAX; 4])
    }
    /// 从四个低位优先的64bit字构造。
    pub const fn from_words(words: [u64; 4]) -> Self {
        Self(words)
    }
    /// 固定四字的集合表示。
    pub const fn words(self) -> [u64; 4] {
        self.0
    }
    /// 域外片段返回false。
    pub const fn contains(self, slot: usize) -> bool {
        slot < 256 && self.0[slot / 64] & (1u64 << (slot % 64)) != 0
    }
    /// 加入0..256片段；域外输入不改变集合。
    pub fn set(&mut self, slot: usize) -> Result<(), Error> {
        if slot >= 256 {
            return Err(Error::Input("分片身份超过256"));
        }
        self.0[slot / 64] |= 1u64 << (slot % 64);
        Ok(())
    }
    /// 集合是否为空。
    pub const fn is_empty(self) -> bool {
        self.0[0] | self.0[1] | self.0[2] | self.0[3] == 0
    }
    /// 递增枚举实际成员，不访问未置位项。
    pub fn iter(self) -> impl Iterator<Item = usize> {
        self.0.into_iter().enumerate().flat_map(|(word, mut bits)| {
            std::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                Some(word * 64 + bit)
            })
        })
    }
}
pub(crate) struct Fragment {
    pub slot: u16,
    pub revision: u64,
    pub mesh: Mesh,
}

/// 只保存非空slab；空region无目录堆分配，数量由所有权变更增量维护。
/// 初始空值共享零身份；首次拥有非空片段后发行独立身份并持续校验版本。
#[derive(Default)]
pub struct MeshParts {
    pub(crate) fragments: Vec<Fragment>,
    len: usize,
    owner: u64,
    version: u64,
}
impl MeshParts {
    /// 已拥有的quad量，O(1)。
    pub const fn len(&self) -> usize {
        self.len
    }
    /// 是否没有几何输出。
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// 非空片段数量。
    pub const fn fragment_count(&self) -> usize {
        self.fragments.len()
    }
    /// 不可变查找；缺失或域外身份返回None。
    pub fn get(&self, slot: usize) -> Option<&Mesh> {
        let slot = u16::try_from(slot).ok()?;
        self.fragments
            .binary_search_by_key(&slot, |v| v.slot)
            .ok()
            .map(|i| &self.fragments[i].mesh)
    }
    /// 按身份顺序借用片段，不复制quad。
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (usize, &Mesh)> {
        self.fragments
            .iter()
            .map(|v| (usize::from(v.slot), &v.mesh))
    }
    /// 替换片段，空Mesh表示移除；失败保持当前所有者，返回被替换的Mesh。
    /// 此处维护数量和布局；有限性和材质域由渲染输入边界校验。
    pub fn insert(&mut self, slot: usize, mesh: Mesh) -> Result<Option<Mesh>, Error> {
        if slot >= 256 {
            return Err(Error::Input("分片身份超过256"));
        }
        let found = self
            .fragments
            .binary_search_by_key(&(slot as u16), |v| v.slot);
        let before = found
            .as_ref()
            .ok()
            .map_or(0, |&i| self.fragments[i].mesh.quads.len());
        let len = (self.len - before)
            .checked_add(mesh.quads.len())
            .ok_or(Error::Input("分片quad数量溢出"))?;
        let version = self
            .version
            .checked_add(1)
            .ok_or(Error::Input("分片版本耗尽"))?;
        if found.is_err() && !mesh.quads.is_empty() {
            self.fragments.try_reserve(1)?;
        }
        let owner = if self.owner == 0 && len != 0 {
            fresh_owner()?
        } else {
            self.owner
        };
        let revision = if mesh.quads.is_empty() {
            0
        } else {
            fresh_owner()?
        };
        self.owner = owner;
        let previous = match found {
            Ok(i) if mesh.quads.is_empty() => Some(self.fragments.remove(i).mesh),
            Ok(i) => {
                self.fragments[i].revision = revision;
                Some(std::mem::replace(&mut self.fragments[i].mesh, mesh))
            }
            Err(i) if !mesh.quads.is_empty() => {
                self.fragments.insert(
                    i,
                    Fragment {
                        slot: slot as u16,
                        revision,
                        mesh,
                    },
                );
                None
            }
            Err(_) => None,
        };
        self.len = len;
        self.version = version;
        Ok(previous)
    }
    /// 移出片段；缺失返回 `Ok(None)`，版本耗尽返回错误且不改变目录或版本。
    pub fn take(&mut self, slot: usize) -> Result<Option<Mesh>, Error> {
        let Ok(slot) = u16::try_from(slot) else {
            return Ok(None);
        };
        let Ok(i) = self.fragments.binary_search_by_key(&slot, |v| v.slot) else {
            return Ok(None);
        };
        let version = self
            .version
            .checked_add(1)
            .ok_or(Error::Input("分片版本耗尽"))?;
        let mesh = self.fragments.remove(i).mesh;
        self.len -= mesh.quads.len();
        self.version = version;
        Ok(Some(mesh))
    }
    /// O(1)借用当前目录，使用所有者缓存的quad量。
    pub fn view(&self) -> MeshView<'_> {
        MeshView::sparse(&self.fragments, self.len)
    }
    /// 目录及quad Vec容量，不含分配器额外开销。
    pub fn retained_bytes(&self) -> usize {
        self.fragments.capacity() * size_of::<Fragment>()
            + self
                .fragments
                .iter()
                .map(|v| v.mesh.quads.capacity() * size_of::<crate::Quad>())
                .sum::<usize>()
    }
}
fn fresh_owner() -> Result<u64, Error> {
    NEXT_OWNER
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .map_err(|_| Error::Input("分片所有者身份耗尽"))
}

/// 消费分片所有者并按身份顺序交还独占Mesh；不移动或复制quad分配。
pub struct IntoFragments {
    inner: std::vec::IntoIter<Fragment>,
}
impl Iterator for IntoFragments {
    type Item = (usize, Mesh);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|v| (usize::from(v.slot), v.mesh))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
impl DoubleEndedIterator for IntoFragments {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner
            .next_back()
            .map(|v| (usize::from(v.slot), v.mesh))
    }
}
impl ExactSizeIterator for IntoFragments {}
impl std::iter::FusedIterator for IntoFragments {}
impl IntoIterator for MeshParts {
    type Item = (usize, Mesh);
    type IntoIter = IntoFragments;
    fn into_iter(self) -> Self::IntoIter {
        IntoFragments {
            inner: self.fragments.into_iter(),
        }
    }
}

/// 片段内连续quad的ARGB颜色候选；实际范围在准备边界验证。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TintPatch {
    fragment: u16,
    reserved: u16,
    first: u32,
    count: u32,
    argb: u32,
}
impl TintPatch {
    /// 校验片段和索引位宽，count必须非零；不借用几何。
    pub fn new(fragment: usize, first: usize, count: usize, argb: u32) -> Result<Self, Error> {
        if fragment >= 256 || count == 0 || first.checked_add(count).is_none() {
            return Err(Error::Input("颜色候选范围无效"));
        }
        Ok(Self {
            fragment: fragment as u16,
            reserved: 0,
            first: u32::try_from(first).map_err(|_| Error::Input("颜色首索引超过u32"))?,
            count: u32::try_from(count).map_err(|_| Error::Input("颜色数量超过u32"))?,
            argb,
        })
    }
    /// 片段身份。
    pub const fn fragment(self) -> usize {
        self.fragment as usize
    }
    /// 片段内首quad。
    pub const fn first(self) -> usize {
        self.first as usize
    }
    /// 连续quad数量。
    pub const fn count(self) -> usize {
        self.count as usize
    }
    /// ARGB乘色；alpha不参与着色。
    pub const fn argb(self) -> u32 {
        self.argb
    }
    pub(crate) fn end(self) -> usize {
        self.first() + self.count()
    }
    pub(crate) fn rgb(self) -> [f32; 3] {
        [16, 8, 0].map(|s| ((self.argb >> s) & 255) as f32 / 255.)
    }
}
const _: () = assert!(size_of::<TintPatch>() == 16);

/// 已校验并预留合并空间的候选；不复制基础几何。
/// 基础身份和版本由管理器patch token保活；GPU成功后finish仅移动已有分配。
pub struct PreparedParts {
    next: MeshParts,
    changed: FragmentMask,
    len: usize,
    merge: Vec<Fragment>,
    tints: Vec<TintPatch>,
    base: (u64, u64),
    old_version: u64,
    next_version: u64,
}
impl PreparedParts {
    /// 校验变化集合并预留移动未改片段的空间；失败不修改old。
    pub fn prepare(
        old: &MeshParts,
        mut next: MeshParts,
        changed: FragmentMask,
    ) -> Result<Self, Error> {
        if next
            .fragments
            .iter()
            .any(|v| !changed.contains(usize::from(v.slot)))
        {
            return Err(Error::Input("候选包含未声明变化的分片"));
        }
        let unchanged = old
            .fragments
            .iter()
            .filter(|v| !changed.contains(usize::from(v.slot)))
            .map(|v| v.mesh.quads.len())
            .sum::<usize>();
        let len = unchanged
            .checked_add(next.len)
            .ok_or(Error::Input("候选quad数量溢出"))?;
        let mut merge = Vec::new();
        merge.try_reserve_exact(old.fragments.len() + next.fragments.len())?;
        if next.owner == 0 && len != 0 {
            next.owner = fresh_owner()?;
        }
        let old_version = old
            .version
            .checked_add(1)
            .ok_or(Error::Input("基础分片版本耗尽"))?;
        let next_version = next
            .version
            .checked_add(1)
            .ok_or(Error::Input("候选分片版本耗尽"))?;
        Ok(Self {
            next,
            changed,
            len,
            merge,
            tints: Vec::new(),
            base: (old.owner, old.version),
            old_version,
            next_version,
        })
    }
    /// 附加颜色候选；完整排序/范围/非重叠校验在GPU准备前完成。
    pub fn with_tints(
        mut self,
        old: &MeshParts,
        mut patches: Vec<TintPatch>,
    ) -> Result<Self, Error> {
        self.check(old);
        patches.sort_unstable_by_key(|p| (p.fragment, p.first));
        let view = self.view(old);
        let mut previous: Option<TintPatch> = None;
        for &p in &patches {
            let mesh = view
                .fragment(p.fragment())
                .ok_or(Error::Input("颜色候选对应分片不存在"))?;
            if p.end() > mesh.quads.len()
                || previous.is_some_and(|a| a.fragment == p.fragment && a.end() > p.first())
            {
                return Err(Error::Input("颜色候选重叠或越界"));
            }
            previous = Some(p);
        }
        self.tints = patches;
        Ok(self)
    }
    fn check(&self, old: &MeshParts) {
        assert_eq!(
            self.base,
            (old.owner, old.version),
            "基础分片已变，候选不可复用"
        );
    }
    /// 包含借用未改片段的逻辑quad量。
    pub const fn len(&self) -> usize {
        self.len
    }
    /// 候选逻辑覆盖是否没有quad。
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// 候选自己拥有的quad量，不重复计算旧借用。
    pub const fn owned_len(&self) -> usize {
        self.next.len
    }
    /// 撤回未发布候选，交还其独占新片段；基础所有者不变。
    /// 调用前须结束GPU准备借用，颜色候选由上层依据持久绑定重新建立。
    pub fn into_owned(self) -> MeshParts {
        self.next
    }
    /// 几何变化位集合。
    pub const fn changed(&self) -> FragmentMask {
        self.changed
    }
    /// 借用候选；基础身份/版本自prepare后变化会panic。
    pub fn view<'a>(&'a self, old: &'a MeshParts) -> MeshView<'a> {
        self.check(old);
        MeshView::overlay_sparse(
            &old.fragments,
            &self.next.fragments,
            self.changed,
            self.len,
            &self.tints,
        )
    }
    /// GPU成功、旧读取结束后移动未改片段并应用颜色；无分配。
    /// old保留旧dirty几何供退役；基础身份/版本变化会panic。
    pub fn finish(mut self, old: &mut MeshParts) -> MeshParts {
        self.check(old);
        let mut carried = old
            .fragments
            .extract_if(.., |v| !self.changed.contains(usize::from(v.slot)))
            .peekable();
        let mut built = self.next.fragments.drain(..).peekable();
        while carried.peek().is_some() || built.peek().is_some() {
            let from_old = match (carried.peek(), built.peek()) {
                (Some(a), Some(b)) => a.slot < b.slot,
                (Some(_), None) => true,
                _ => false,
            };
            self.merge.push(if from_old {
                carried.next().expect("已检查旧片段")
            } else {
                built.next().expect("已检查新片段")
            });
        }
        drop(carried);
        drop(built);
        old.len = old.fragments.iter().map(|v| v.mesh.quads.len()).sum();
        old.version = self.old_version;
        std::mem::swap(&mut self.next.fragments, &mut self.merge);
        self.next.len = self.len;
        self.next.version = self.next_version;
        for p in self.tints {
            let i = self
                .next
                .fragments
                .binary_search_by_key(&p.fragment, |v| v.slot)
                .expect("已验证颜色分片");
            for q in &mut self.next.fragments[i].mesh.quads[p.first()..p.end()] {
                q.set_tint(p.rgb());
            }
        }
        self.next
    }
    /// 候选保留容量；包含新几何、合并目录、颜色，不含旧借用。
    pub fn retained_bytes(&self) -> usize {
        self.next.retained_bytes()
            + self.merge.capacity() * size_of::<Fragment>()
            + self.tints.capacity() * size_of::<TintPatch>()
    }
}

#[cfg(test)]
#[path = "mesh_parts_tests.rs"]
mod tests;
