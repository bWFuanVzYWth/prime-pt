//! 变化批先验证再排队；真实block依赖只在world传播一次。
use super::*;
use crate::SourceChange;

#[derive(Default)]
pub(super) struct TintSweep {
    active: bool,
    cursor: Option<[i32; 3]>,
    again: bool,
}
impl TintSweep {
    fn start(&mut self) {
        if self.active {
            self.again = true;
        } else {
            self.active = true;
            self.cursor = None;
        }
    }
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) const fn pending(&self) -> bool {
        self.active
    }
}

impl Scene {
    /// 整批验证后入队；`plan` 按本轮维护额度消费，旧发布覆盖始终有效。
    pub fn apply_changes(&mut self, changes: &[SourceChange]) -> Result<(), Error> {
        for change in changes {
            match *change {
                SourceChange::BlockRange {
                    section,
                    minimum,
                    maximum,
                } => {
                    prime_minecraft::dependent_slabs(section, minimum, maximum)?;
                }
                SourceChange::SectionSource(value) => {
                    validate_section(value)?;
                }
                SourceChange::ColumnSource {
                    x,
                    z,
                    minimum_y,
                    maximum_y,
                    geometry,
                    biome,
                } => {
                    validate_section([x, minimum_y, z])?;
                    if minimum_y >= maximum_y
                        || maximum_y.checked_mul(16).is_none()
                        || !(geometry || biome)
                    {
                        return Err(Error::Input("来源列范围或类别无效"));
                    }
                }
                SourceChange::BiomeColumn { x, z } => {
                    validate_section([x, 0, z])?;
                }
                SourceChange::WorldTint | SourceChange::SourceOverflow => {}
            }
        }
        self.queued_changes.try_reserve(changes.len())?;
        let next_raw = self
            .pending_raw
            .checked_add(changes.len())
            .ok_or(Error::Input("通知数量溢出"))?;
        let mut keys = std::collections::BTreeSet::new();
        for &change in changes {
            match change {
                SourceChange::BlockRange { section, .. } | SourceChange::SectionSource(section) => {
                    keys.insert(section);
                }
                SourceChange::ColumnSource {
                    x,
                    z,
                    minimum_y,
                    maximum_y,
                    geometry: true,
                    ..
                } => {
                    keys.extend(
                        self.compact
                            .store
                            .keys_in_column(x, z, minimum_y, maximum_y),
                    );
                    keys.extend(
                        self.captures
                            .iter()
                            .map(|capture| capture.wire.section)
                            .filter(|key| {
                                key[0] == x
                                    && key[2] == z
                                    && key[1] >= minimum_y
                                    && key[1] < maximum_y
                            }),
                    );
                }
                _ => {}
            }
        }
        if self.compact.generation != 0 {
            self.compact
                .store
                .invalidate_batch(&keys.iter().copied().collect::<Vec<_>>())?;
        }
        // 通知边界立即撤销旧wire；维护额度只决定何时再次捕获，不允许晚回复越过失效。
        self.captures
            .retain(|capture| !keys.contains(&capture.wire.section));
        if self.compact.generation == 0 {
            for job in &mut self.jobs {
                for key in &keys {
                    if region(*key) == job.region {
                        job.issued &= !(1 << slot(*key));
                    }
                }
            }
        }
        if changes.contains(&SourceChange::SourceOverflow) {
            self.invalidate_input_epoch();
            self.compact.clear(self.compact.generation);
            self.captures.clear();
        }
        self.pending_raw = next_raw;
        self.queued_changes.extend(changes.iter().copied());
        self.planner_pending |= !changes.is_empty();
        Ok(())
    }

    pub(super) fn process_changes(&mut self) -> Result<(), Error> {
        self.progress.raw_invalidations += std::mem::take(&mut self.pending_raw);
        let mut sources = BTreeMap::<[i32; 3], ([u8; 3], [u8; 3])>::new();
        let mut tint_all = false;
        let mut biome_columns = std::collections::BTreeSet::new();
        for _ in 0..self.work_budget.maintenance {
            let Some(change) = self.queued_changes.pop_front() else {
                break;
            };
            match change {
                SourceChange::BlockRange {
                    section,
                    minimum,
                    maximum,
                } => merge_range(&mut sources, section, minimum, maximum),
                SourceChange::SectionSource(section) => {
                    merge_range(&mut sources, section, [0; 3], [16; 3])
                }
                SourceChange::ColumnSource {
                    x,
                    z,
                    minimum_y,
                    maximum_y,
                    geometry,
                    biome,
                } => {
                    if geometry {
                        for key in self
                            .compact
                            .store
                            .keys_in_column(x, z, minimum_y, maximum_y)
                        {
                            merge_range(&mut sources, key, [0; 3], [16; 3]);
                        }
                    }
                    if biome {
                        biome_columns.insert((x, z));
                    }
                }
                SourceChange::BiomeColumn { x, z } => {
                    biome_columns.insert((x, z));
                }
                SourceChange::WorldTint => tint_all = true,
                SourceChange::SourceOverflow => {
                    self.invalidations
                        .extend(self.state.occupied().map(|(handle, _)| handle));
                    self.scan = (!self.invalidations.is_empty()).then_some(0);
                    tint_all = true;
                }
            }
        }
        self.progress.unique_invalidations +=
            sources.len() + biome_columns.len() + usize::from(tint_all);
        let source_changed = !sources.is_empty();
        for (source, (minimum, maximum)) in sources {
            if self.compact.generation != 0 {
                self.compact.refresh_source(source)?;
            } else {
                self.dirty_source_output(source, minimum, maximum)?;
            }
        }
        if tint_all {
            self.retint_all()?;
        } else {
            for (x, z) in biome_columns {
                self.retint_column(x, z)?;
            }
            if source_changed {
                self.retint_custom()?;
            }
        }
        self.advance_tint_sweep(false)?;
        self.advance_tint_sweep(true)?;
        Ok(())
    }

    pub(super) fn dirty_source_output(
        &mut self,
        source: [i32; 3],
        minimum: [u8; 3],
        maximum: [u8; 3],
    ) -> Result<(), Error> {
        if self.compact.generation == 0 {
            if let Some(handle) = self.state.find(chunk(region(source))) {
                self.invalidate_fragments(handle, fragments(1 << slot(source)))?;
            }
            return Ok(());
        }
        let mut regions = BTreeMap::<[i32; 3], FragmentMask>::new();
        for dependency in prime_minecraft::dependent_slabs(source, minimum, maximum)? {
            if dependency.section[1] < self.height[0] || dependency.section[1] >= self.height[1] {
                continue;
            }
            let region = region(dependency.section);
            let mask = regions.entry(region).or_default();
            let section = slot(dependency.section);
            for slab in 0..4 {
                if dependency.slabs & (1 << slab) != 0 {
                    mask.set(section * 4 + slab)?;
                }
            }
        }
        for (key, mask) in regions {
            if let Some(handle) = self.state.find(chunk(key)) {
                self.invalidate_fragments(handle, mask)?;
            }
        }
        Ok(())
    }

    fn invalidate_fragments(&mut self, handle: Handle, mask: FragmentMask) -> Result<(), Error> {
        if let Some(index) = self.handle_job_index(handle) {
            let missing = self
                .state
                .resource(handle)
                .map_or(0, |old| old.missing_sections)
                | self.jobs[index].unavailable;
            let affected = fragment_sections(mask);
            let mask = union(mask, fragments(missing & affected));
            let ticket = self.jobs[index].ticket;
            let cancelled_tints = self.compact.cancel_fragments(ticket, mask);
            let job = &mut self.jobs[index];
            job.pending_tints -= cancelled_tints;
            job.pending_tint_mask = subtract(job.pending_tint_mask, mask);
            job.sections.retain(|(fragment, mesh)| {
                if mask.contains(*fragment) {
                    self.ledger.owned -= mesh.quads.len();
                    false
                } else {
                    true
                }
            });
            job.bindings.retain(|entry| !mask.contains(entry.fragment));
            job.tint_patches
                .retain(|patch| !mask.contains(patch.fragment()));
            job.color_updates
                .retain(|entry| !mask.contains(entry.fragment));
            job.changed = union(job.changed, mask);
            job.compiled = subtract(job.compiled, mask);
            job.verify = subtract(job.verify, mask);
            job.finished = subtract(job.finished, mask);
            job.unavailable &= !affected;
            job.dirty = fragment_sections(job.changed);
            job.received = 0;
            for section in 0..64 {
                refresh_received(job, section);
            }
            self.progress.cancelled_slabs += mask.iter().count();
            if self.compact.generation != 0 {
                self.compact.register(&self.jobs[index])?;
            }
            self.queue_completion(ticket);
            self.refresh_job_projection(index);
        } else {
            self.mark_fragments(handle, mask, FragmentMask::empty());
            self.reject_handle(handle)?;
            self.recover_candidate(handle)?;
            self.state.invalidate(handle)?;
            self.refresh_ledger(handle);
        }
        self.planner_pending = true;
        Ok(())
    }

    fn recover_candidate(&mut self, handle: Handle) -> Result<(), Error> {
        // 待卸载的可见节点只有当前资源；保留卸载锁，失效标记仍可用于重新进入视距。
        if self.state.is_visible(handle) && self.state.replacement(handle).is_none() {
            return Ok(());
        }
        if let Some(resource) = self.state.take_unpublished(handle)? {
            let key = resource.key;
            self.remove_candidate_sources(key);
            let previous = self.recovered.insert(key, resource.into_partial());
            assert!(previous.is_none(), "一个节点最多一份未发布所有者");
        }
        Ok(())
    }

    fn reject_handle(&mut self, handle: Handle) -> Result<(), Error> {
        let Some(value) = self.state.chunk(handle) else {
            return Ok(());
        };
        let mut index = 0;
        while index < self.pending.len() {
            let matched = matches!(self.pending[index].change, Change::Activate(key) | Change::Replace(key) | Change::Remove(key) if key == value);
            if matched {
                let patch = self.pending.swap_remove(index);
                self.reject_token(patch)?;
            } else {
                index += 1;
            }
        }
        self.view_changes.clear();
        self.view_len = self.view.len();
        Ok(())
    }

    fn retint_all(&mut self) -> Result<(), Error> {
        if self.compact.generation == 0 {
            self.invalidations
                .extend(self.state.occupied().map(|(handle, _)| handle));
            self.scan = (!self.invalidations.is_empty()).then_some(0);
            return Ok(());
        }
        self.tint_sweep.start();
        Ok(())
    }

    fn retint_column(&mut self, x: i32, z: i32) -> Result<(), Error> {
        let bounds = self.state.config().bounds().expect("有界视域");
        let low = [
            i64::from(x - 1).div_euclid(4),
            bounds.minimum()[1],
            i64::from(z - 1).div_euclid(4),
        ];
        let high = [
            i64::from(x + 1).div_euclid(4) + 1,
            bounds.maximum()[1],
            i64::from(z + 1).div_euclid(4) + 1,
        ];
        let entries: Vec<_> = self
            .tinted_spatial
            .keys(Bounds::new(low, high)?)
            .into_iter()
            .filter_map(|key| self.tinted.get(&key).copied().map(|mask| (key, mask)))
            .collect();
        for (key, mask) in entries {
            let mut affected = FragmentMask::empty();
            for fragment in mask.iter() {
                let source = section(key, fragment / 4);
                if source[0].abs_diff(x) <= 1 && source[2].abs_diff(z) <= 1 {
                    affected.set(fragment)?;
                }
            }
            if !affected.is_empty() {
                self.mark_tints(key, affected)?;
            }
        }
        Ok(())
    }

    fn retint_custom(&mut self) -> Result<(), Error> {
        self.custom_sweep.start();
        Ok(())
    }

    fn advance_tint_sweep(&mut self, custom: bool) -> Result<(), Error> {
        use std::ops::Bound::{Excluded, Unbounded};
        let (sweep, index) = if custom {
            (&mut self.custom_sweep, &self.custom_tinted)
        } else {
            (&mut self.tint_sweep, &self.tinted)
        };
        if !sweep.active || self.work_budget.maintenance == 0 {
            return Ok(());
        }
        let start = sweep.cursor.map_or(Unbounded, Excluded);
        let entries: Vec<_> = index
            .range((start, Unbounded))
            .take(self.work_budget.maintenance)
            .map(|(key, mask)| (*key, *mask))
            .collect();
        let last = entries.last().map(|(key, _)| *key).or(sweep.cursor);
        let complete =
            last.is_none_or(|last| index.range((Excluded(last), Unbounded)).next().is_none());
        if complete {
            sweep.active = sweep.again;
            sweep.again = false;
            sweep.cursor = None;
        } else {
            sweep.cursor = last;
        }
        for (key, mask) in entries {
            self.mark_tints(key, mask)?;
        }
        Ok(())
    }

    fn mark_tints(&mut self, key: [i32; 3], mask: FragmentMask) -> Result<(), Error> {
        let Some(handle) = self.state.find(chunk(key)) else {
            return Ok(());
        };
        let missing = self
            .state
            .replacement(handle)
            .or_else(|| self.state.resource(handle))
            .map_or(0, |region| region.missing_sections);
        if let Some(index) = self.handle_job_index(handle) {
            let job = &mut self.jobs[index];
            let mask = subtract(mask, fragments(job.unavailable | (missing & !job.dirty)));
            if mask.is_empty() {
                return Ok(());
            }
            job.tint_mask = union(job.tint_mask, mask);
            job.pending_tints -= self.compact.cancel_tints(job.ticket, mask);
            job.pending_tint_mask = subtract(job.pending_tint_mask, mask);
            job.tint_patches
                .retain(|patch| !mask.contains(patch.fragment()));
            job.color_updates
                .retain(|entry| !mask.contains(entry.fragment));
            // tint_mask与pending_tints独立阻止发布，已完成的几何和源验证继续有效。
            let ticket = job.ticket;
            self.queue_tints(ticket)?;
        } else {
            let mask = subtract(mask, fragments(missing));
            if mask.is_empty() {
                return Ok(());
            }
            self.reject_handle(handle)?;
            self.recover_candidate(handle)?;
            self.mark_fragments(handle, FragmentMask::empty(), mask);
            self.state.invalidate(handle)?;
        }
        self.planner_pending = true;
        Ok(())
    }
}

pub(super) fn refresh_received(job: &mut RegionJob, section: usize) {
    if (section * 4..section * 4 + 4)
        .all(|fragment| !job.changed.contains(fragment) || job.finished.contains(fragment))
    {
        job.received |= (1 << section) & job.dirty;
    }
}

pub(super) fn union(a: FragmentMask, b: FragmentMask) -> FragmentMask {
    FragmentMask::from_words(std::array::from_fn(|word| {
        a.words()[word] | b.words()[word]
    }))
}
pub(super) fn subtract(a: FragmentMask, b: FragmentMask) -> FragmentMask {
    FragmentMask::from_words(std::array::from_fn(|word| {
        a.words()[word] & !b.words()[word]
    }))
}
fn validate_section(key: [i32; 3]) -> Result<(), Error> {
    if key.iter().any(|v| v.checked_mul(16).is_none()) {
        Err(Error::Input("来源section坐标越界"))
    } else {
        Ok(())
    }
}
fn merge_range(
    map: &mut BTreeMap<[i32; 3], ([u8; 3], [u8; 3])>,
    key: [i32; 3],
    minimum: [u8; 3],
    maximum: [u8; 3],
) {
    map.entry(key)
        .and_modify(|(low, high)| {
            for axis in 0..3 {
                low[axis] = low[axis].min(minimum[axis]);
                high[axis] = high[axis].max(maximum[axis]);
            }
        })
        .or_insert((minimum, maximum));
}
