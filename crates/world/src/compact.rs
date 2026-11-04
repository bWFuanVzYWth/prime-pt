//! 压缩源、持久乘色绑定和显式slab阶段；只借用源，输出由World独占。
use super::*;
use crate::priority::PriorityQueue;
use crate::store::{SourceStamp, SourceState, SourceStore, WorkId};
use prime_minecraft::{Catalog, Neighbor, Neighborhood, TintRequest};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
#[path = "compile_ready.rs"]
mod compile_ready;
#[cfg(test)]
#[path = "job_capacity_tests.rs"]
mod job_capacity_tests;
#[cfg(test)]
#[path = "source_progress_tests.rs"]
mod source_progress_tests;
#[cfg(test)]
#[path = "unavailable_tests.rs"]
mod unavailable_tests;

#[derive(Default)]
pub(super) struct Sources {
    pub generation: u64,
    pub store: SourceStore,
    pub tints: Vec<PendingTint>,
    pub queries: Vec<TintRequest>,
    pub indices: HashMap<TintRequest, usize>,
    registrations: HashMap<Ticket, BTreeMap<usize, WorkId>>,
    groups: HashMap<WorkId, Ticket>,
    todo: PriorityQueue<WorkId>,
    ready: PriorityQueue<WorkId>,
    expanded: HashSet<WorkId>,
    capacity: BTreeMap<WorkId, (usize, bool)>,
    capacity_index: [BTreeSet<(usize, WorkId)>; 2],
    minimums: HashMap<WorkId, usize>,
    probes: HashMap<[i32; 3], WorkId>,
    probe_spatial: crate::spatial::SpatialIndex,
    probe_window: Option<Bounds>,
    edit_capture_turn: u8,
    next_epoch: u64,
}
pub(super) struct PendingTint {
    pub ticket: Ticket,
    pub fragment: usize,
    pub old: bool,
    pub patches: Vec<(usize, usize)>,
}

/// 兼容测试的中心请求顺序；生产直接由注册事件建立Store依赖。
#[cfg(test)]
pub(super) fn needed(region: [i32; 3], dirty: u64) -> Vec<[i32; 3]> {
    (0..64)
        .filter(|slot| dirty & (1 << slot) != 0)
        .map(|slot| section(region, slot))
        .collect()
}

impl Sources {
    pub(super) fn clear(&mut self, generation: u64) {
        self.store.clear(generation);
        self.generation = generation;
        self.tints.clear();
        self.queries.clear();
        self.indices.clear();
        self.registrations.clear();
        self.groups.clear();
        self.todo.clear();
        self.ready.clear();
        self.expanded.clear();
        self.capacity.clear();
        for index in &mut self.capacity_index {
            index.clear();
        }
        self.minimums.clear();
        self.probes.clear();
        self.probe_spatial.clear();
        self.probe_window = None;
        self.edit_capture_turn = 0;
    }
    pub(super) fn ready_len(&self) -> usize {
        self.ready.len()
    }
    pub(super) fn capture_owner(&self, key: [i32; 3]) -> Option<Ticket> {
        self.groups.get(&self.store.consumer(key)?).copied()
    }
    pub(super) fn refresh_source(&mut self, key: [i32; 3]) -> Result<(), Error> {
        if self.store.consumer(key).is_some() {
            return Ok(());
        }
        self.probe_source(key)?;
        Ok(())
    }

    pub(super) fn refresh_edit(&mut self, key: [i32; 3]) -> Result<(), Error> {
        // 编辑probe独立于建网ticket，重排或取消已有job不丢失本次捕获。
        if self.probe_source(key)? {
            self.store.promote_edit(key)?;
        }
        Ok(())
    }

    fn probe_source(&mut self, key: [i32; 3]) -> Result<bool, Error> {
        if !matches!(self.store.lookup(key), SourceState::Unknown)
            || self
                .probe_window
                .is_some_and(|bounds| !crate::spatial::contains(bounds, key))
        {
            return Ok(false);
        }
        if self.probes.contains_key(&key) {
            return Ok(true);
        }
        let epoch = self
            .next_epoch
            .checked_add(1)
            .ok_or(Error::Input("source工作身份耗尽"))?;
        let probe = WorkId {
            region: key,
            fragment: 0,
            epoch,
        };
        self.store.depend(probe, &[key])?;
        self.next_epoch = epoch;
        self.probes.insert(key, probe);
        self.probe_spatial.insert(key);
        Ok(true)
    }
    pub(super) fn set_window(&mut self, bounds: Bounds) {
        if self.probe_window == Some(bounds) {
            return;
        }
        let exiting: Vec<_> = if let Some(previous) = self.probe_window {
            crate::spatial::difference(previous, bounds)
                .into_iter()
                .flat_map(|edge| self.probe_spatial.keys(edge))
                .collect()
        } else {
            self.probes
                .keys()
                .copied()
                .filter(|key| !crate::spatial::contains(bounds, *key))
                .collect()
        };
        for key in exiting {
            if let Some(work) = self.probes.remove(&key) {
                self.store.forget(work);
            }
            self.probe_spatial.remove(key);
        }
        self.probe_window = Some(bounds);
        self.store.set_window(bounds);
    }
    pub(super) fn job_sources(&self, ticket: Ticket) -> Vec<([i32; 3], SourceStamp)> {
        let mut keys = BTreeSet::new();
        if let Some(groups) = self.registrations.get(&ticket) {
            for &work in groups.values() {
                keys.extend(self.store.dependencies(work).iter().copied());
            }
        }
        keys.into_iter()
            .map(|key| (key, self.store.stamp(key)))
            .collect()
    }
    fn remove_capacity(&mut self, work: WorkId) {
        if let Some((required, interactive)) = self.capacity.remove(&work) {
            self.capacity_index[usize::from(interactive)].remove(&(required, work));
        }
    }

    pub(super) fn promote_job(&mut self, ticket: Ticket) -> Result<(), Error> {
        let Some(groups) = self.registrations.get(&ticket) else {
            return Ok(());
        };
        for &group in groups.values() {
            self.todo.promote(group)?;
            for offset in 0..4 {
                let work = WorkId {
                    fragment: group.fragment + offset,
                    ..group
                };
                self.ready.promote(work)?;
                if let Some((required, false)) = self.capacity.get(&work).copied() {
                    self.capacity_index[0].remove(&(required, work));
                    self.capacity_index[1].insert((required, work));
                    self.capacity.insert(work, (required, true));
                }
            }
        }
        Ok(())
    }
    pub(super) fn forget_job(&mut self, ticket: Ticket) {
        if let Some(groups) = self.registrations.remove(&ticket) {
            for (_, group) in groups {
                self.store.forget(group);
                self.groups.remove(&group);
                self.todo.remove(&group);
                self.expanded.remove(&group);
                for offset in 0..4 {
                    let work = WorkId {
                        fragment: group.fragment + offset,
                        ..group
                    };
                    self.ready.remove(&work);
                    self.remove_capacity(work);
                    self.minimums.remove(&work);
                }
            }
        }
        self.tints.retain(|tint| tint.ticket != ticket);
        self.prune_tints();
    }
    pub(super) fn cancel_fragments(
        &mut self,
        ticket: Ticket,
        mask: FragmentMask,
        interactive: bool,
    ) -> Result<usize, Error> {
        if let Some(groups) = self.registrations.get(&ticket) {
            for fragment in mask.iter() {
                if let Some(&group) = groups.get(&(fragment / 4)) {
                    let work = WorkId {
                        fragment: fragment as u16,
                        ..group
                    };
                    self.ready.remove(&work);
                    if let Some((required, lane)) = self.capacity.remove(&work) {
                        self.capacity_index[usize::from(lane)].remove(&(required, work));
                    }
                    self.minimums.remove(&work);
                    self.todo.insert(group, interactive)?;
                }
            }
        }
        let before = self.tints.len();
        self.tints
            .retain(|tint| tint.ticket != ticket || !mask.contains(tint.fragment));
        let cancelled = before - self.tints.len();
        if cancelled != 0 {
            self.prune_tints();
        }
        Ok(cancelled)
    }
    pub(super) fn cancel_tints(&mut self, ticket: Ticket, mask: FragmentMask) -> usize {
        let before = self.tints.len();
        self.tints
            .retain(|tint| tint.ticket != ticket || !mask.contains(tint.fragment));
        let cancelled = before - self.tints.len();
        if cancelled != 0 {
            self.prune_tints();
        }
        cancelled
    }
    pub(super) fn register(&mut self, job: &RegionJob) -> Result<(), Error> {
        // 同一ticket的dirty只取并集，分组仅在整job退休时删除；数量相等即已注册完整。
        if self.registrations.get(&job.ticket).map_or(0, BTreeMap::len)
            == job.dirty.count_ones() as usize
        {
            return Ok(());
        }
        let mut dirty = job.dirty;
        while dirty != 0 {
            let slot = dirty.trailing_zeros() as usize;
            dirty &= dirty - 1;
            if job.received & (1 << slot) != 0 {
                continue;
            }
            if self
                .registrations
                .get(&job.ticket)
                .is_some_and(|groups| groups.contains_key(&slot))
            {
                continue;
            }
            let epoch = self
                .next_epoch
                .checked_add(1)
                .ok_or(Error::Input("source工作身份耗尽"))?;
            let work = WorkId {
                region: job.region,
                fragment: (slot * 4) as u16,
                epoch,
            };
            self.store.depend(work, &[section(job.region, slot)])?;
            self.next_epoch = epoch;
            self.registrations
                .entry(job.ticket)
                .or_default()
                .insert(slot, work);
            self.groups.insert(work, job.ticket);
            self.todo.insert(work, job.interactive)?;
        }
        Ok(())
    }
    fn query(&mut self, request: TintRequest) -> usize {
        if let Some(&index) = self.indices.get(&request) {
            return index;
        }
        let index = self.queries.len();
        self.queries.push(request);
        self.indices.insert(request, index);
        index
    }
    pub(super) fn prune_tints(&mut self) {
        if self.tints.is_empty() {
            self.queries.clear();
            self.indices.clear();
            return;
        }
        let mut remap = vec![usize::MAX; self.queries.len()];
        for tint in &self.tints {
            for &(_, color) in &tint.patches {
                remap[color] = 0;
            }
        }
        let mut count = 0;
        for (index, value) in remap.iter_mut().enumerate() {
            if *value != usize::MAX {
                *value = count;
                self.queries[count] = self.queries[index];
                count += 1;
            }
        }
        self.queries.truncate(count);
        self.indices.clear();
        self.indices.extend(
            self.queries
                .iter()
                .enumerate()
                .map(|(index, &value)| (value, index)),
        );
        for tint in &mut self.tints {
            for (_, color) in &mut tint.patches {
                *color = remap[*color];
            }
        }
    }
}

/// 预留完整世代的CPU State；借用锁住原Scene，丢弃时不改变已发布资源。
/// ```compile_fail,E0499
/// fn stale(scene: &mut prime_world::Scene) {
///     let prepared = scene.stage_source_generation(2).unwrap();
///     scene.clear();
///     prepared.commit();
/// }
/// ```
pub struct PreparedGeneration<'a> {
    scene: &'a mut Scene,
    generation: u64,
    state: Option<State<RegionMesh>>,
}
impl PreparedGeneration<'_> {
    /// GPU事务成功后仅移动预留状态；无分配或可恢复失败。
    pub fn commit(self) {
        if let Some(state) = self.state {
            self.scene.clear_with(state);
            self.scene.compact.clear(self.generation);
        }
    }
}
impl Scene {
    pub(super) fn issue_compact_captures(&mut self, count: usize) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        if let Some(edit) = self.compact.store.pick_edit(|_| true) {
            // Java可以只完成前缀的一条。每八个有编辑的推进轮让普通捕获占首位一次。
            let first = if self.compact.edit_capture_turn == 7 {
                self.captures
                    .iter()
                    .find(|capture| !self.compact.store.is_edited_missing(capture.wire.section))
                    .map(|capture| capture.wire.section)
                    .or_else(|| self.compact.store.pick_background(|_| true))
                    .unwrap_or(edit)
            } else {
                edit
            };
            if let Some(index) = self
                .captures
                .iter()
                .position(|capture| capture.wire.section == first)
            {
                self.captures[..=index].rotate_right(1);
            } else {
                let capture = self.new_compact_capture(first)?;
                if self.captures.len() == REQUESTS {
                    self.captures.pop();
                }
                self.captures.insert(0, capture);
            }
            self.compact.edit_capture_turn = (self.compact.edit_capture_turn + 1) % 8;
        }
        while self.captures.len() < count {
            let eligible = |key| {
                !self
                    .captures
                    .iter()
                    .any(|capture| capture.wire.section == key)
            };
            let Some(key) = self
                .compact
                .store
                .pick_background(eligible)
                .or_else(|| self.compact.store.pick_edit(eligible))
            else {
                break;
            };
            let capture = self.new_compact_capture(key)?;
            self.captures.push(capture);
        }
        Ok(())
    }

    fn new_compact_capture(&mut self, key: [i32; 3]) -> Result<PendingCapture, Error> {
        let next = self
            .next_wire
            .checked_add(1)
            .ok_or(Error::Input("捕获请求编号溢出"))?;
        let ticket = self.compact.capture_owner(key);
        let source_stamp = self
            .compact
            .store
            .capture_stamp(key)
            .ok_or(Error::Input("签发的source已不再缺失"))?;
        self.compact.store.capture_issued(key)?;
        let wire = CaptureRequest {
            id: self.next_wire,
            section: key,
            reserved: 0,
        };
        self.next_wire = next;
        Ok(PendingCapture {
            wire,
            ticket,
            slot: 0,
            source_stamp: Some(source_stamp),
        })
    }

    /// GPU资源替换前预留完整世界源域；零世代拒绝，失败不改变旧所有者。
    pub fn stage_source_generation(
        &mut self,
        generation: u64,
    ) -> Result<PreparedGeneration<'_>, Error> {
        if generation == 0 {
            return Err(Error::Input("压缩源资源世代为零"));
        }
        let state = if self.compact.generation == generation {
            None
        } else {
            Some(State::new(configuration(
                initial_bounds(),
                Target::new([0; 3])?,
                INITIAL_CAPACITY,
            )?))
        };
        Ok(PreparedGeneration {
            scene: self,
            generation,
            state,
        })
    }
    /// 无GPU事务的便利入口；生产先stage，再共同提交资源与世界。
    pub fn source_generation(&mut self, generation: u64) -> Result<(), Error> {
        self.stage_source_generation(generation)?.commit();
        Ok(())
    }
    /// 当前完整模型/压缩源世代；零只用于兼容自足网格输入。
    pub const fn generation(&self) -> u64 {
        self.compact.generation
    }
    /// 先验证整包票据、坐标、世代和 Catalog 身份，再共同接受原始压缩数据。
    /// 旧票据忽略。等价快照恢复可用性并保留内容版本，实际状态差分精确唤醒受影响候选。
    /// 持有本轮准备视图的宿主须先结束 GPU 准备和发布，再交付后续源包。
    pub fn apply_compact(&mut self, bytes: &[u8], catalog: &Catalog) -> Result<(), Error> {
        let sources = prime_minecraft::decode_sections(bytes)?;
        let mut accepted = Vec::new();
        accepted.try_reserve_exact(sources.len())?;
        let mut tickets = Vec::new();
        tickets.try_reserve_exact(sources.len())?;
        for source in sources {
            let Some(capture) = self
                .captures
                .iter()
                .find(|capture| capture.wire.id == source.request)
            else {
                continue;
            };
            if capture.wire.section != source.key
                || source.generation != self.compact.generation
                || source.generation != catalog.generation()
            {
                return Err(Error::Input("压缩源票据、坐标或资源世代不匹配"));
            }
            let Some(stamp) = capture.source_stamp else {
                return Err(Error::Input("压缩源票据缺少失效身份"));
            };
            if self.compact.store.capture_stamp(source.key) != Some(stamp) {
                tickets.push(source.request);
                continue;
            }
            let air = if let Some(storage) = &source.storage {
                catalog.validate_section(storage)?;
                catalog.section_is_air(storage)?
            } else {
                false
            };
            tickets.push(source.request);
            accepted.push((source, air, stamp));
        }
        let deltas = self.compact.store.accept_batch(accepted)?;
        self.captures
            .retain(|capture| !tickets.contains(&capture.wire.id));
        for delta in deltas {
            if let Some(probe) = self.compact.probes.remove(&delta.key) {
                self.compact.store.forget(probe);
                self.compact.probe_spatial.remove(delta.key);
            }
            if let Some(range) = delta.changed {
                self.dirty_source_output_with_priority(
                    delta.key,
                    range.min,
                    range.max_exclusive,
                    delta.interactive,
                )?;
            }
        }
        self.compile_sources(catalog)
    }
    /// 持久来源按完整输入键去重后的颜色请求；原版与必要自定义回调共同续接。
    pub fn tint_requests(&self) -> impl ExactSizeIterator<Item = TintRequest> + '_ {
        self.compact.queries.iter().copied()
    }
    /// 完整回填；先构造全部颜色候选，再改变job，旧发布几何始终不变。
    pub fn apply_tints(&mut self, colors: &[u32]) -> Result<(), Error> {
        if colors.len() != self.compact.queries.len() {
            return Err(Error::Input("world tint响应数量不匹配"));
        }
        let mut updates = Vec::new();
        updates.try_reserve_exact(self.compact.tints.len())?;
        for pending in &self.compact.tints {
            let job = self
                .job_index(pending.ticket)
                .map(|index| &self.jobs[index])
                .ok_or(Error::Input("tint候选已取消"))?;
            let binding = if pending.old {
                self.state.resource(job.handle).and_then(|region| {
                    region
                        .bindings
                        .iter()
                        .find(|entry| entry.fragment == pending.fragment)
                })
            } else {
                job.bindings
                    .iter()
                    .find(|entry| entry.fragment == pending.fragment)
            }
            .ok_or(Error::Input("tint持久来源缺失"))?;
            let mut patches = Vec::new();
            patches.try_reserve_exact(pending.patches.len())?;
            for &(span, query) in &pending.patches {
                let binding_span = binding
                    .spans
                    .get(span)
                    .ok_or(Error::Input("tint span索引越界"))?;
                let argb = *colors.get(query).ok_or(Error::Input("tint响应索引越界"))?;
                let previous = binding
                    .colors
                    .get(span)
                    .ok_or(Error::Input("tint旧颜色索引越界"))?;
                if !pending.old || argb != *previous {
                    patches.push((
                        prime_scene::TintPatch::new(
                            pending.fragment,
                            binding_span.first as usize,
                            binding_span.count as usize,
                            argb,
                        )?,
                        span,
                        argb,
                    ));
                }
            }
            updates.push(patches);
        }
        for (pending, patches) in self.compact.tints.iter().zip(&updates) {
            let index = self.job_index(pending.ticket).expect("已验证候选身份");
            let job = &mut self.jobs[index];
            job.tint_patches.try_reserve(patches.len())?;
            if pending.old {
                job.color_updates.try_reserve(patches.len())?;
            }
        }
        let mut pending_tints = std::mem::take(&mut self.compact.tints);
        for (pending, patches) in pending_tints.drain(..).zip(updates) {
            let index = self.job_index(pending.ticket).expect("上方已验证候选身份");
            let job = &mut self.jobs[index];
            for (patch, span, argb) in patches {
                job.tint_patches.push(patch);
                if pending.old {
                    job.color_updates.push(BindingColor {
                        fragment: pending.fragment,
                        span,
                        argb,
                    });
                } else {
                    job.bindings
                        .iter_mut()
                        .find(|entry| entry.fragment == pending.fragment)
                        .expect("已验证来源")
                        .colors[span] = argb;
                }
            }
            // 恢复片段的源依赖由advance_sources确认，颜色回填不能代替该检查。
            if !pending.old && !job.verify.contains(pending.fragment) {
                finish_fragment(job, pending.fragment)?;
            }
            job.pending_tints = job
                .pending_tints
                .checked_sub(1)
                .expect("待色计数与唯一pending所有者一致");
            let mut mask = job.pending_tint_mask.words();
            mask[pending.fragment / 64] &= !(1 << (pending.fragment % 64));
            job.pending_tint_mask = FragmentMask::from_words(mask);
            self.queue_completion(pending.ticket);
        }
        self.compact.tints = pending_tints;
        self.compact.queries.clear();
        self.compact.indices.clear();
        self.finish_jobs()?;
        self.planner_pending = true;
        Ok(())
    }
    fn neighborhood<'a>(&'a self, key: [i32; 3], generation: u64) -> Neighborhood<'a> {
        neighborhood_from(&self.compact.store, self.height, key, generation)
    }
    fn stage_old_tints(&mut self, catalog: &Catalog) -> Result<(), Error> {
        while self.tint_remaining != 0 {
            let Some(ticket) = self.tint_jobs.pop_first() else {
                break;
            };
            let Some(index) = self.job_index(ticket) else {
                continue;
            };
            if self.jobs[index].invalidated || Some(self.jobs[index].epoch) != self.input_epoch {
                continue;
            }
            let mask = self.jobs[index].tint_mask;
            let mut remaining = mask.words();
            let mut requeue = false;
            for fragment in mask.iter() {
                let clear = |words: &mut [u64; 4]| {
                    words[fragment / 64] &= !(1 << (fragment % 64));
                };
                let job = &self.jobs[index];
                if job.pending_tint_mask.contains(fragment) {
                    clear(&mut remaining);
                    continue;
                }
                let old = !job.changed.contains(fragment);
                if !old && !job.compiled.contains(fragment) {
                    continue;
                }
                let binding = if old {
                    self.state.resource(job.handle).and_then(|region| {
                        region
                            .bindings
                            .iter()
                            .find(|entry| entry.fragment == fragment)
                    })
                } else {
                    job.bindings.iter().find(|entry| entry.fragment == fragment)
                };
                let Some(binding) = binding else {
                    clear(&mut remaining);
                    continue;
                };
                if self.tint_remaining == 0 {
                    requeue = true;
                    break;
                }
                self.tint_remaining -= 1;
                self.tint_used += 1;
                let key = section(job.region, fragment / 4);
                let mut requests = Vec::new();
                requests.try_reserve_exact(binding.spans.len())?;
                for &span in &binding.spans {
                    requests.push(catalog.tint_request(self.compact.generation, key, span)?);
                }
                let patches = requests
                    .into_iter()
                    .enumerate()
                    .map(|(span, request)| (span, self.compact.query(request)))
                    .collect();
                self.compact.tints.push(PendingTint {
                    ticket: self.jobs[index].ticket,
                    fragment,
                    old,
                    patches,
                });
                self.jobs[index].pending_tints += 1;
                self.jobs[index].pending_tint_mask.set(fragment)?;
                clear(&mut remaining);
            }
            self.jobs[index].tint_mask = FragmentMask::from_words(remaining);
            if requeue {
                self.tint_jobs
                    .insert(ticket, self.jobs[index].interactive)?;
            }
            self.queue_completion(ticket);
        }
        Ok(())
    }
    /// 只推进实际可执行队列；同section四个slab共享一次邻域借用，无嵌套调度。
    /// 每轮遵守内核数量额度，Air证明不占token；缺源或压力保留旧覆盖。
    pub fn compile_sources(&mut self, catalog: &Catalog) -> Result<(), Error> {
        if self.compact.generation == 0 {
            return Ok(());
        }
        if catalog.generation() != self.compact.generation {
            return Err(Error::Input("world/Catalog世代不匹配"));
        }
        self.stage_old_tints(catalog)?;
        self.advance_sources()?;
        self.compile_ready(catalog)?;
        self.finish_jobs()?;
        Ok(())
    }
}
fn finish_fragment(job: &mut RegionJob, fragment: usize) -> Result<(), Error> {
    job.finished.set(fragment)?;
    let mut verify = job.verify.words();
    verify[fragment / 64] &= !(1 << (fragment % 64));
    job.verify = FragmentMask::from_words(verify);
    let slot = fragment / 4;
    if (slot * 4..slot * 4 + 4)
        .all(|fragment| !job.changed.contains(fragment) || job.finished.contains(fragment))
    {
        job.received |= 1 << slot;
    }
    Ok(())
}

fn neighborhood_from(
    store: &SourceStore,
    height: [i32; 2],
    key: [i32; 3],
    generation: u64,
) -> Neighborhood<'_> {
    Neighborhood {
        key,
        generation,
        sections: std::array::from_fn(|index| {
            let offset = [index % 3, index / 9, index / 3 % 3].map(|v| v as i32 - 1);
            let position = std::array::from_fn(|axis| key[axis] + offset[axis]);
            if position[1] < height[0] || position[1] >= height[1] {
                return Neighbor::Empty;
            }
            match store.lookup(position) {
                SourceState::Available(storage) => Neighbor::Available(storage),
                SourceState::Air => Neighbor::Empty,
                SourceState::Unavailable => Neighbor::Unavailable,
                SourceState::Unknown => Neighbor::Unknown,
            }
        }),
    }
}
