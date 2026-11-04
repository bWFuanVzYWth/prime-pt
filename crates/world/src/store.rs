//! 压缩源与编译依赖的唯一所有者。64 个 section 共用一个 region 页；已证明 air
//! 只占位图与版本，不保留 palette。失效源仍独占旧数据用于实际差分，编译只借用有效源。

use crate::ready_queue::ReadyQueue;
use crate::{Error, spatial};
use chunk_manager::Bounds;
use prime_minecraft::{SectionSource, SectionStorage};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

pub(crate) type Key = [i32; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceStamp {
    pub generation: u64,
    pub version: u64,
}

/// 同一世界源域内，捕获签发时该 section 的失效身份。它与内容版本独立。
/// 只和 Scene 的唯一在途 wire 一起验证，单独的印记不能授权回复。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CaptureStamp(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalRange {
    pub min: [u8; 3],
    pub max_exclusive: [u8; 3],
}

impl LocalRange {
    pub const FULL: Self = Self {
        min: [0; 3],
        max_exclusive: [16; 3],
    };
}

pub(crate) struct SourceDelta {
    pub key: Key,
    pub changed: Option<LocalRange>,
    /// 接受之前的调度类别。它不参与内容判断和源版本。
    pub interactive: bool,
}

/// epoch 由候选所有者签发，不能把复用 fragment 槽误认为旧工作；这不是资源引用计数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WorkId {
    pub region: Key,
    pub fragment: u16,
    pub epoch: u64,
}

pub(crate) enum SourceState<'a> {
    Unknown,
    Unavailable,
    Air,
    Available(&'a SectionStorage),
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SourceStats {
    pub sources: usize,
    pub pages: usize,
    pub raw_bytes: usize,
    pub dependency_edges: usize,
    pub maintenance_steps: usize,
}

struct Page {
    present: u64,
    known: u64,
    valid: u64,
    unavailable: u64,
    air: u64,
    versions: [u64; 64],
    storage: HashMap<u8, SectionStorage>,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            present: 0,
            known: 0,
            valid: 0,
            unavailable: 0,
            air: 0,
            versions: [0; 64],
            storage: HashMap::new(),
        }
    }
}

#[derive(Default)]
pub(crate) struct SourceStore {
    generation: u64,
    serial: u64,
    invalidations: HashMap<Key, CaptureStamp>,
    pages: BTreeMap<Key, Page>,
    spatial: spatial::SpatialIndex,
    reverse: HashMap<Key, HashSet<WorkId>>,
    works: HashMap<WorkId, Vec<Key>>,
    missing: BTreeSet<(u64, Key)>,
    missing_by_key: HashMap<Key, u64>,
    missing_age: ReadyQueue<Key>,
    edited_missing: ReadyQueue<Key>,
    capture_turn: u8,
    woken: BTreeSet<WorkId>,
    window: Option<Bounds>,
    retiring: VecDeque<Key>,
    retiring_set: HashSet<Key>,
    retired: Vec<Key>,
    stats: SourceStats,
}

impl SourceStore {
    /// 完整世界/资源域切换；版本序号不归零，不能复用旧 stamp。
    pub fn clear(&mut self, generation: u64) {
        let serial = self.serial;
        *self = Self {
            generation,
            serial,
            ..Self::default()
        };
    }

    pub fn lookup(&self, key: Key) -> SourceState<'_> {
        let (region, slot) = address(key);
        let Some(page) = self.pages.get(&region) else {
            return SourceState::Unknown;
        };
        let bit = 1 << slot;
        if page.valid & bit == 0 {
            SourceState::Unknown
        } else if page.unavailable & bit != 0 {
            SourceState::Unavailable
        } else if page.air & bit != 0 {
            SourceState::Air
        } else {
            SourceState::Available(page.storage.get(&slot).expect("有效非air源拥有压缩存储"))
        }
    }

    /// 内容身份。未接受过的键版本为零；暂时失效或重捕获到同样内容保留原版本。
    /// 发布还须确认来源当前可用，不能单凭版本判定有效。
    pub fn stamp(&self, key: Key) -> SourceStamp {
        let (region, slot) = address(key);
        SourceStamp {
            generation: self.generation,
            version: self
                .pages
                .get(&region)
                .map_or(0, |p| p.versions[usize::from(slot)]),
        }
    }

    /// 捕获只签发给尚未得到有效快照的键。每次新失效撤销此前签发的身份。
    pub fn capture_stamp(&self, key: Key) -> Option<CaptureStamp> {
        matches!(self.lookup(key), SourceState::Unknown).then(|| {
            self.invalidations
                .get(&key)
                .copied()
                .unwrap_or(CaptureStamp(0))
        })
    }

    #[cfg(test)]
    pub fn accept(&mut self, source: SectionSource, air: bool) -> Result<SourceDelta, Error> {
        let stamp = self.capture_stamp(source.key).expect("测试签发缺源请求");
        Ok(self
            .accept_batch(vec![(source, air, stamp)])?
            .pop()
            .expect("单源事务"))
    }

    /// 调用者先验证整包票据、坐标及 Catalog 身份；这里再次封闭世代/版本与压缩所有权。
    /// 所有可恢复失败都在写入任何源内容之前；等价快照仅恢复有效性，保留旧表示和内容版本。
    pub fn accept_batch(
        &mut self,
        sources: Vec<(SectionSource, bool, CaptureStamp)>,
    ) -> Result<Vec<SourceDelta>, Error> {
        let mut seen = HashSet::new();
        seen.try_reserve(sources.len())?;
        let mut staged = Vec::new();
        staged.try_reserve_exact(sources.len())?;
        let mut deltas = Vec::new();
        deltas.try_reserve_exact(sources.len())?;
        let mut end = self.serial;
        for (source, air, stamp) in sources {
            if source.generation == 0
                || source.generation != self.generation
                || self.capture_stamp(source.key) != Some(stamp)
                || air && source.storage.is_none()
                || !seen.insert(source.key)
            {
                return Err(Error::Input(
                    "source事务世代、失效身份、air证明或唯一键无效",
                ));
            }
            let (region, slot) = address(source.key);
            let changed = self
                .pages
                .get(&region)
                .map_or(Some(LocalRange::FULL), |page| {
                    difference(page, slot, source.storage.as_ref(), air)
                });
            let version = if changed.is_some() {
                end = end
                    .checked_add(1)
                    .ok_or(Error::Input("source版本序号耗尽"))?;
                Some(end)
            } else {
                None
            };
            deltas.push(SourceDelta {
                key: source.key,
                changed,
                interactive: self.is_edited_missing(source.key),
            });
            staged.push((source, air, version));
        }
        // 页与 payload 的扩容也在提交前完成；只改变保留容量，不改变可观察源内容。
        let mut new_pages = BTreeMap::<Key, Page>::new();
        let mut payloads = HashMap::<Key, usize>::new();
        payloads.try_reserve(staged.len())?;
        for (source, air, version) in &staged {
            if version.is_some() && !air && source.storage.is_some() {
                *payloads.entry(address(source.key).0).or_default() += 1;
            }
        }
        for (source, _, _) in &staged {
            let (region, _) = address(source.key);
            if !self.pages.contains_key(&region) {
                new_pages.entry(region).or_default();
            }
        }
        for (region, count) in payloads {
            let page = if let Some(page) = self.pages.get_mut(&region) {
                page
            } else {
                new_pages.get_mut(&region).expect("已预备新增页")
            };
            page.storage.try_reserve(count)?;
        }
        for (region, page) in new_pages {
            self.pages.insert(region, page);
            self.spatial.insert(region);
        }
        self.serial = end;
        for (source, air, version) in staged {
            let key = source.key;
            let (region, slot) = address(key);
            let page = self.pages.get_mut(&region).expect("事务已预备页");
            let bit = 1 << slot;
            if page.present & bit == 0 {
                self.stats.sources += 1;
            }
            page.present |= bit;
            page.known |= bit;
            page.valid |= bit;
            if let Some(version) = version {
                page.versions[usize::from(slot)] = version;
                set(&mut page.air, bit, air);
                set(&mut page.unavailable, bit, source.storage.is_none());
                if let Some(old) = page.storage.remove(&slot) {
                    self.stats.raw_bytes -= old.source_bytes();
                }
                if let Some(storage) = source.storage.filter(|_| !air) {
                    self.stats.raw_bytes += storage.source_bytes();
                    page.storage.insert(slot, storage);
                }
            }
            self.invalidations.remove(&key);
            self.refresh_missing(key);
            self.wake(key);
            self.schedule_retirement(key);
        }
        self.stats.pages = self.pages.len();
        Ok(deltas)
    }

    /// 仅使已捕获数据不可被编译使用；旧 payload 留给下一次真实响应作状态差分。
    fn invalidate_reserved(&mut self, key: Key, stamp: CaptureStamp) {
        let (region, slot) = address(key);
        let page = self.pages.entry(region).or_insert_with(|| {
            self.spatial.insert(region);
            Page::default()
        });
        let bit = 1 << slot;
        if page.present & bit == 0 {
            self.stats.sources += 1;
        }
        page.present |= bit;
        page.valid &= !bit;
        self.invalidations.insert(key, stamp);
        self.stats.pages = self.pages.len();
        self.refresh_missing(key);
        self.wake(key);
        self.schedule_retirement(key);
    }

    #[cfg(test)]
    fn invalidate(&mut self, key: Key) -> Result<(), Error> {
        self.invalidate_batch(&[key])
    }

    /// 先预留整个唯一键集合的版本域，再使全部旧来源不可读；不部分消费输入批。
    pub fn invalidate_batch(&mut self, keys: &[Key]) -> Result<(), Error> {
        let mut unique = Vec::new();
        unique.try_reserve_exact(keys.len())?;
        unique.extend_from_slice(keys);
        unique.sort_unstable();
        unique.dedup();
        self.missing_age.reserve_insertions(unique.len())?;
        self.invalidations.try_reserve(unique.len())?;
        let end = self
            .serial
            .checked_add(unique.len() as u64)
            .ok_or(Error::Input("source版本序号耗尽"))?;
        for (index, key) in unique.into_iter().enumerate() {
            self.invalidate_reserved(key, CaptureStamp(self.serial + index as u64 + 1));
        }
        self.serial = end;
        Ok(())
    }

    /// 只枚举实际存在页内的同列来源；不按世界高度或视距体积生成空键。
    pub fn keys_in_column(&self, x: i32, z: i32, minimum_y: i32, maximum_y: i32) -> Vec<Key> {
        if minimum_y >= maximum_y {
            return Vec::new();
        }
        let low = [
            i64::from(x).div_euclid(4),
            i64::from(minimum_y).div_euclid(4),
            i64::from(z).div_euclid(4),
        ];
        let high = [
            low[0] + 1,
            (i64::from(maximum_y) - 1).div_euclid(4) + 1,
            low[2] + 1,
        ];
        let mut keys = Vec::new();
        for region in self
            .spatial
            .keys(Bounds::new(low, high).expect("非空列页域"))
        {
            let mut bits = self.pages[&region].present;
            while bits != 0 {
                let slot = bits.trailing_zeros() as u8;
                bits &= bits - 1;
                let key = section(region, slot);
                if key[0] == x && key[2] == z && key[1] >= minimum_y && key[1] < maximum_y {
                    keys.push(key);
                }
            }
        }
        keys
    }

    /// 完整预留后替换一个候选的依赖边；错误保留旧图，重复键不增加边数。
    pub fn depend(&mut self, work: WorkId, keys: &[Key]) -> Result<(), Error> {
        if work.epoch == 0 || work.fragment >= 256 {
            return Err(Error::Input("source候选依赖身份无效"));
        }
        let mut unique = Vec::new();
        unique.try_reserve_exact(keys.len())?;
        unique.extend_from_slice(keys);
        unique.sort_unstable();
        unique.dedup();
        self.missing_age.reserve_insertions(unique.len())?;
        self.works.try_reserve(1)?;
        self.reverse.try_reserve(unique.len())?;
        let mut new = Vec::new();
        new.try_reserve_exact(unique.len())?;
        for &key in &unique {
            if let Some(waiters) = self.reverse.get_mut(&key) {
                waiters.try_reserve(1)?;
            } else {
                let mut waiters = HashSet::new();
                waiters.try_reserve(1)?;
                new.push((key, waiters));
            }
        }
        self.woken.remove(&work);
        let old = self.works.remove(&work).unwrap_or_default();
        self.stats.dependency_edges -= old.len();
        for key in &old {
            self.reverse
                .get_mut(key)
                .expect("旧工作拥有反向依赖")
                .remove(&work);
        }
        self.reverse.extend(new);
        for &key in &unique {
            self.reverse
                .get_mut(&key)
                .expect("已预留依赖集合")
                .insert(work);
            self.refresh_missing(key);
        }
        self.stats.dependency_edges += unique.len();
        self.works.insert(work, unique);
        for key in old {
            if self.reverse.get(&key).is_some_and(HashSet::is_empty) {
                self.reverse.remove(&key);
            }
            self.refresh_missing(key);
            self.schedule_retirement(key);
        }
        Ok(())
    }

    pub fn forget(&mut self, work: WorkId) {
        self.woken.remove(&work);
        if let Some(keys) = self.works.remove(&work) {
            self.stats.dependency_edges -= keys.len();
            for key in keys {
                if let Some(waiters) = self.reverse.get_mut(&key) {
                    waiters.remove(&work);
                    if waiters.is_empty() {
                        self.reverse.remove(&key);
                    }
                }
                self.refresh_missing(key);
                self.schedule_retirement(key);
            }
        }
    }

    /// 每项工作最多一个唤醒；已取消 epoch 的队列项不会返回给调用者。
    pub fn drain_woken(&mut self, max: usize) -> impl Iterator<Item = WorkId> + '_ {
        let mut left = max;
        std::iter::from_fn(move || {
            if left == 0 {
                return None;
            }
            let work = self.woken.pop_first()?;
            left -= 1;
            Some(work)
        })
    }

    /// 只列仍有普通消费者且从未捕获/已失效的键；编辑队列使用独立队龄。
    #[cfg(test)]
    pub fn missing(&self) -> impl ExactSizeIterator<Item = Key> + '_ {
        self.missing.iter().map(|(_, key)| *key)
    }

    /// 普通来源七次优先最早真实消费者，随后一次按公平队龄。两个索引均不包含编辑源。
    pub fn pick_background(&self, mut eligible: impl FnMut(Key) -> bool) -> Option<Key> {
        if self.capture_turn == 7 {
            self.missing_age.iter().copied().find(|key| eligible(*key))
        } else {
            self.missing
                .iter()
                .map(|(_, key)| *key)
                .find(|key| eligible(*key))
        }
    }

    /// 保留同一缺源键首次编辑的队龄，源确认或最后一个依赖撤销时删除。
    pub fn promote_edit(&mut self, key: Key) -> Result<(), Error> {
        if let Some(&priority) = self.missing_by_key.get(&key)
            && self.edited_missing.insert(key)?
        {
            self.missing.remove(&(priority, key));
            self.missing_age.remove(&key);
        }
        Ok(())
    }

    pub fn pick_edit(&self, mut eligible: impl FnMut(Key) -> bool) -> Option<Key> {
        self.edited_missing
            .iter()
            .copied()
            .find(|key| eligible(*key))
    }

    pub fn is_edited_missing(&self, key: Key) -> bool {
        self.edited_missing.contains(&key)
    }

    /// 实际签发才轮转公平队龄；重复通知不改年龄，已知后重新缺失则排到队尾。
    pub fn capture_issued(&mut self, key: Key) -> Result<(), Error> {
        if !self.missing_by_key.contains_key(&key) {
            return Err(Error::Input("签发的source已不再缺失"));
        }
        if self.is_edited_missing(key) {
            return Ok(());
        }
        if !self.missing_age.renew(&key)? {
            return Err(Error::Input("缺源双索引身份不一致"));
        }
        self.capture_turn = (self.capture_turn + 1) % 8;
        Ok(())
    }

    /// 一个仍持有该源依赖的工作，用于给共享捕获选择真实存活票据。
    pub fn consumer(&self, key: Key) -> Option<WorkId> {
        self.reverse.get(&key)?.iter().next().copied()
    }

    pub fn dependencies(&self, work: WorkId) -> &[Key] {
        self.works.get(&work).map_or(&[], Vec::as_slice)
    }

    /// 仅枚举旧窗退出前沿中的实际页；不按视距体积或全部 source 做 retain。
    pub fn set_window(&mut self, bounds: Bounds) {
        let pages = if let Some(old) = self.window {
            let mut pages = HashSet::new();
            for frontier in spatial::difference(old, bounds) {
                pages.extend(self.spatial.keys(page_bounds(frontier)));
            }
            pages.into_iter().collect::<Vec<_>>()
        } else {
            self.pages.keys().copied().collect()
        };
        self.window = Some(bounds);
        for region in pages {
            let mut mask = self.pages[&region].present;
            while mask != 0 {
                let slot = mask.trailing_zeros() as u8;
                mask &= mask - 1;
                self.schedule_retirement(section(region, slot));
            }
        }
    }

    /// 每次最多处理 max 个实际退休候选；仍有编译依赖或已重入窗口的源保留。
    pub fn maintain(&mut self, max: usize) -> usize {
        let mut count = 0;
        for _ in 0..max {
            let Some(key) = self.retiring.pop_front() else {
                break;
            };
            self.stats.maintenance_steps = self.stats.maintenance_steps.saturating_add(1);
            self.retiring_set.remove(&key);
            if self.reverse.contains_key(&key) || !self.outside(key) {
                continue;
            }
            let (region, slot) = address(key);
            let Some(page) = self.pages.get_mut(&region) else {
                continue;
            };
            let bit = 1 << slot;
            if page.present & bit == 0 {
                continue;
            }
            if let Some(storage) = page.storage.remove(&slot) {
                self.stats.raw_bytes -= storage.source_bytes();
            }
            page.present &= !bit;
            page.known &= !bit;
            page.valid &= !bit;
            page.air &= !bit;
            page.unavailable &= !bit;
            page.versions[usize::from(slot)] = 0;
            self.invalidations.remove(&key);
            self.stats.sources -= 1;
            if page.present == 0 {
                self.pages.remove(&region);
                self.spatial.remove(region);
            }
            self.retired.push(key);
            count += 1;
        }
        self.stats.pages = self.pages.len();
        count
    }

    pub fn drain_retired(&mut self) -> impl Iterator<Item = Key> + '_ {
        self.retired.drain(..)
    }

    pub fn stats(&self) -> SourceStats {
        self.stats
    }

    fn outside(&self, key: Key) -> bool {
        self.window
            .is_some_and(|bounds| !spatial::contains(bounds, key))
    }

    fn schedule_retirement(&mut self, key: Key) {
        if self.outside(key) && self.retiring_set.insert(key) {
            self.retiring.push_back(key);
        }
    }

    fn refresh_missing(&mut self, key: Key) {
        if let Some(priority) = self.missing_by_key.remove(&key) {
            self.missing.remove(&(priority, key));
        }
        if matches!(self.lookup(key), SourceState::Unknown)
            && let Some(priority) = self
                .reverse
                .get(&key)
                .and_then(|works| works.iter().map(|work| work.epoch).min())
        {
            self.missing_by_key.insert(key, priority);
            if !self.edited_missing.contains(&key) {
                self.missing.insert((priority, key));
                self.missing_age
                    .insert(key)
                    .expect("依赖或失效边界已预留完整缺源队龄");
            }
        } else {
            self.missing_age.remove(&key);
            self.edited_missing.remove(&key);
        }
    }

    fn wake(&mut self, key: Key) {
        if let Some(works) = self.reverse.get(&key) {
            for &work in works {
                self.woken.insert(work);
            }
        }
    }
}

fn set(mask: &mut u64, bit: u64, enabled: bool) {
    if enabled { *mask |= bit } else { *mask &= !bit }
}

fn address(key: Key) -> (Key, u8) {
    let local = key.map(|v| v.rem_euclid(4) as u8);
    (
        key.map(|v| v.div_euclid(4)),
        local[0] * 16 + local[1] * 4 + local[2],
    )
}

fn section(region: Key, slot: u8) -> Key {
    let local = [slot / 16, slot / 4 % 4, slot % 4];
    std::array::from_fn(|axis| region[axis] * 4 + i32::from(local[axis]))
}

fn page_bounds(bounds: Bounds) -> Bounds {
    Bounds::new(
        bounds.minimum().map(|v| v.div_euclid(4)),
        bounds.maximum().map(|v| (v - 1).div_euclid(4) + 1),
    )
    .expect("非空section域的region页域非空")
}

fn difference(
    page: &Page,
    slot: u8,
    next: Option<&SectionStorage>,
    air: bool,
) -> Option<LocalRange> {
    let bit = 1 << slot;
    if page.known & bit == 0 {
        return Some(LocalRange::FULL);
    }
    let old_unavailable = page.unavailable & bit != 0;
    if old_unavailable || next.is_none() {
        return (old_unavailable != next.is_none()).then_some(LocalRange::FULL);
    }
    let old_air = page.air & bit != 0;
    if old_air || air {
        return (old_air != air).then_some(LocalRange::FULL);
    }
    let old = page.storage.get(&slot).expect("旧非air数据保留到差分完成");
    let next = next.expect("已处理Unavailable");
    old.changed_bounds(next)
        .map(|(min, max_exclusive)| LocalRange { min, max_exclusive })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(key: Key, state: Option<u32>) -> SectionSource {
        SectionSource {
            request: 1,
            key,
            generation: 1,
            storage: state.map(|id| SectionStorage::new(0, vec![id], vec![]).unwrap()),
        }
    }

    fn store() -> SourceStore {
        let mut store = SourceStore::default();
        store.clear(1);
        store
    }

    fn work(epoch: u64) -> WorkId {
        WorkId {
            region: [0; 3],
            fragment: 5,
            epoch,
        }
    }

    #[test]
    fn invalidation_keeps_stale_data_for_exact_actual_state_difference() {
        let mut store = store();
        store.accept(source([0; 3], Some(1)), false).unwrap();
        let previous = store.stamp([0; 3]);
        store.invalidate([0; 3]).unwrap();
        assert!(matches!(store.lookup([0; 3]), SourceState::Unknown));
        assert_eq!(store.stamp([0; 3]), previous);
        let mut same = source([0; 3], None);
        same.storage = Some(SectionStorage::new(1, vec![1, 2], vec![0; 64]).unwrap());
        assert!(store.accept(same, false).unwrap().changed.is_none());
        assert_eq!(store.stamp([0; 3]), previous);
        store.invalidate([0; 3]).unwrap();
        let mut changed = source([0; 3], None);
        let mut words = vec![0; 64];
        words[1537 / 64] |= 1 << (1537 % 64);
        changed.storage = Some(SectionStorage::new(1, vec![1, 2], words).unwrap());
        assert_eq!(
            store.accept(changed, false).unwrap().changed,
            Some(LocalRange {
                min: [1, 6, 0],
                max_exclusive: [2, 7, 1]
            })
        );
        assert!(store.stamp([0; 3]).version > previous.version);
    }

    #[test]
    fn another_invalidation_rejects_old_capture_without_changing_content_identity() {
        let mut store = store();
        let key = [0; 3];
        store.accept(source(key, Some(1)), false).unwrap();
        let content = store.stamp(key);
        store.invalidate(key).unwrap();
        let old = store.capture_stamp(key).unwrap();
        store.invalidate(key).unwrap();
        let latest = store.capture_stamp(key).unwrap();
        assert_ne!(old, latest);
        assert!(
            store
                .accept_batch(vec![(source(key, Some(2)), false, old)])
                .is_err()
        );
        assert_eq!(store.stamp(key), content);
        assert!(matches!(store.lookup(key), SourceState::Unknown));
    }

    #[test]
    fn full_batch_failure_does_not_publish_a_prefix_or_reuse_versions() {
        let mut store = store();
        let mut wrong = source([1; 3], Some(1));
        wrong.generation = 2;
        assert!(
            store
                .accept_batch(vec![
                    (source([0; 3], Some(1)), false, CaptureStamp(0)),
                    (wrong, false, CaptureStamp(0))
                ])
                .is_err()
        );
        assert_eq!(store.stats().sources, 0);
        assert_eq!(store.stamp([0; 3]).version, 0);
        store.accept(source([0; 3], Some(1)), false).unwrap();
        let stamp = store.stamp([0; 3]);
        store.clear(1);
        store.accept(source([0; 3], Some(1)), false).unwrap();
        assert!(store.stamp([0; 3]).version > stamp.version);
        store.serial = u64::MAX - 1;
        assert!(
            store
                .accept_batch(vec![
                    (source([1; 3], Some(1)), false, CaptureStamp(0)),
                    (source([2; 3], Some(1)), false, CaptureStamp(0))
                ])
                .is_err()
        );
        assert!(matches!(store.lookup([1; 3]), SourceState::Unknown));
    }

    #[test]
    fn air_uses_only_page_bits_and_unavailable_remains_an_explicit_negative_cache() {
        let mut store = store();
        for x in 0..4 {
            for y in 0..4 {
                for z in 0..4 {
                    store.accept(source([x, y, z], Some(0)), true).unwrap();
                }
            }
        }
        assert_eq!(store.stats().pages, 1);
        assert_eq!(store.stats().sources, 64);
        assert_eq!(store.stats().raw_bytes, 0);
        assert!(matches!(store.lookup([3; 3]), SourceState::Air));
        store.depend(work(1), &[[4; 3]]).unwrap();
        assert_eq!(store.missing().collect::<Vec<_>>(), [[4; 3]]);
        store.accept(source([4; 3], None), false).unwrap();
        assert!(matches!(store.lookup([4; 3]), SourceState::Unavailable));
        assert_eq!(store.missing().len(), 0);
        store.invalidate([4; 3]).unwrap();
        assert_eq!(store.missing().len(), 1);
    }

    #[test]
    fn dependency_replacement_deduplicates_edges_and_discards_cancelled_work() {
        let mut store = store();
        store.depend(work(1), &[[0; 3], [0; 3], [1; 3]]).unwrap();
        assert_eq!(store.stats().dependency_edges, 2);
        store.depend(work(1), &[[0; 3], [2; 3]]).unwrap();
        assert_eq!(store.stats().dependency_edges, 2);
        store.accept(source([0; 3], Some(0)), true).unwrap();
        store.accept(source([2; 3], Some(0)), true).unwrap();
        assert_eq!(store.drain_woken(8).collect::<Vec<_>>(), [work(1)]);
        store.invalidate([0; 3]).unwrap();
        store.forget(work(1));
        store.depend(work(2), &[[0; 3]]).unwrap();
        store.accept(source([0; 3], Some(0)), true).unwrap();
        assert_eq!(store.drain_woken(8).collect::<Vec<_>>(), [work(2)]);
        assert_eq!(store.stats().dependency_edges, 1);
    }

    #[test]
    fn window_retirement_is_bounded_and_preserves_actual_live_dependencies() {
        let mut store = store();
        let old = Bounds::new([0; 3], [4; 3]).unwrap();
        let next = Bounds::new([4, 0, 0], [8, 4, 4]).unwrap();
        store.set_window(old);
        store.accept(source([0; 3], Some(1)), false).unwrap();
        store.accept(source([1; 3], Some(0)), true).unwrap();
        store.depend(work(1), &[[0; 3]]).unwrap();
        store.set_window(next);
        assert_eq!(store.maintain(1), 0);
        assert_eq!(store.maintain(1), 1);
        assert!(matches!(store.lookup([0; 3]), SourceState::Available(_)));
        store.forget(work(1));
        assert_eq!(store.maintain(1), 1);
        assert_eq!(store.stats().pages, 0);
        assert_eq!(store.stats().raw_bytes, 0);
        let mut retired: Vec<_> = store.drain_retired().collect();
        retired.sort_unstable();
        assert_eq!(retired, [[0; 3], [1; 3]]);
    }

    #[test]
    fn partial_page_frontier_retires_only_sections_that_exit_the_exact_window() {
        let mut store = store();
        store.set_window(Bounds::new([0; 3], [6; 3]).unwrap());
        for key in [[0, 1, 1], [1; 3], [5, 1, 1]] {
            store.accept(source(key, Some(0)), true).unwrap();
        }
        store.set_window(Bounds::new([1; 3], [5; 3]).unwrap());
        assert_eq!(store.maintain(64), 2);
        assert!(matches!(store.lookup([1; 3]), SourceState::Air));
        assert_eq!(store.stats().sources, 1);
        assert_eq!(store.stats().pages, 1);
    }

    #[test]
    fn missing_sources_keep_consumer_age_ahead_of_new_negative_coordinates() {
        let mut store = store();
        store.depend(work(1), &[[100; 3]]).unwrap();
        store.depend(work(2), &[[-100; 3]]).unwrap();
        store.depend(work(3), &[[100; 3]]).unwrap();
        assert_eq!(store.missing().collect::<Vec<_>>(), [[100; 3], [-100; 3]]);
        store.forget(work(1));
        assert_eq!(
            store.missing().collect::<Vec<_>>(),
            [[-100; 3], [100; 3]],
            "最早消费者退休后按剩余真实消费者重排"
        );
    }

    #[test]
    fn one_request_budget_reserves_eighth_slot_for_far_source_despite_near_churn() {
        let mut store = store();
        let near = [0; 3];
        let far = [100; 3];
        store.depend(work(1), &[near]).unwrap();
        store.depend(work(2), &[far]).unwrap();
        for _ in 0..7 {
            assert_eq!(store.pick_background(|_| true), Some(near));
            store.capture_issued(near).unwrap();
            store.invalidate(near).unwrap();
        }
        assert_eq!(
            store.pick_background(|_| false),
            None,
            "在途或零可用输出不消耗公平槽"
        );
        assert_eq!(store.pick_background(|key| key != far), Some(near));
        assert_eq!(
            store.pick_background(|_| true),
            Some(far),
            "预算1跨调用累计，重复近源不能重置公平年龄"
        );
        store.capture_issued(far).unwrap();
        assert_eq!(store.pick_background(|_| true), Some(near));
    }
}
