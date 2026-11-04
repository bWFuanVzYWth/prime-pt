//! 数量、推进额度与容量增长跟随唯一所有者；不通过全cut快照计算预算。
use super::*;

pub(super) fn fragments(sections: u64) -> FragmentMask {
    if sections == u64::MAX {
        return FragmentMask::all();
    }
    let mut words = [0; 4];
    let mut sections = sections;
    while sections != 0 {
        let section = sections.trailing_zeros() as usize;
        sections &= sections - 1;
        words[section / 16] |= 15 << ((section % 16) * 4);
    }
    FragmentMask::from_words(words)
}

pub(super) fn sections(fragments: FragmentMask) -> u64 {
    fragments
        .iter()
        .fold(0, |bits, fragment| bits | (1 << (fragment / 4)))
}

impl Scene {
    pub(super) fn job_index(&self, ticket: Ticket) -> Option<usize> {
        self.job_tickets.get(&ticket).copied()
    }
    pub(super) fn handle_job_index(&self, handle: Handle) -> Option<usize> {
        self.job_handles.get(&handle).copied()
    }
    pub(super) fn queue_completion(&mut self, ticket: Ticket) {
        if let Some(index) = self.job_index(ticket) {
            self.completion_jobs
                .insert(self.jobs[index].handle.slot(), ticket);
        }
    }
    pub(super) fn queue_tints(&mut self, ticket: Ticket) -> Result<(), Error> {
        if let Some(index) = self.job_index(ticket)
            && !self.jobs[index].tint_mask.is_empty()
        {
            self.tint_jobs.insert(ticket)?;
        }
        Ok(())
    }
    pub(super) fn insert_job(&mut self, job: RegionJob) -> Result<(), Error> {
        self.job_tickets.try_reserve(1)?;
        self.job_handles.try_reserve(1)?;
        self.jobs.try_reserve(1)?;
        let index = self.jobs.len();
        let ticket = job.ticket;
        let handle = job.handle;
        self.job_tickets.insert(ticket, index);
        self.job_handles.insert(handle, index);
        self.jobs.push(job);
        if self.compact.generation != 0
            && let Err(error) = self.compact.register(&self.jobs[index])
        {
            let job = self.drop_job(index);
            self.state.fail(job.ticket)?;
            return Err(error);
        }
        self.queue_completion(ticket);
        self.queue_tints(ticket)?;
        Ok(())
    }
    pub(super) fn remove_job(&mut self, index: usize) -> RegionJob {
        let job = self.jobs.swap_remove(index);
        self.job_tickets.remove(&job.ticket);
        self.job_handles.remove(&job.handle);
        self.completion_jobs.remove(&job.handle.slot());
        self.tint_jobs.remove(&job.ticket);
        if let Some(moved) = self.jobs.get(index) {
            self.job_tickets.insert(moved.ticket, index);
            self.job_handles.insert(moved.handle, index);
        }
        job
    }
    pub(super) fn remove_candidate_sources(&mut self, region: [i32; 3]) {
        if let Some(sources) = self.candidate_sources.remove(&region) {
            for (source, _) in sources {
                let dependents = self
                    .candidate_dependents
                    .get_mut(&source)
                    .expect("候选反向边存在");
                dependents.remove(&region);
                if dependents.is_empty() {
                    self.candidate_dependents.remove(&source);
                }
            }
        }
        self.candidate_epochs.remove(&region);
    }

    pub(super) fn install_candidate_sources(
        &mut self,
        region: [i32; 3],
        sources: Vec<([i32; 3], crate::store::SourceStamp)>,
    ) {
        self.remove_candidate_sources(region);
        for &(source, _) in &sources {
            self.candidate_dependents
                .entry(source)
                .or_default()
                .insert(region);
        }
        self.candidate_sources.insert(region, sources);
    }

    pub(super) fn rebind_candidate_source(
        &mut self,
        source: [i32; 3],
        stamp: crate::store::SourceStamp,
    ) {
        if let Some(regions) = self.candidate_dependents.get(&source) {
            for region in regions {
                let sources = self
                    .candidate_sources
                    .get_mut(region)
                    .expect("候选正向边存在");
                let at = sources
                    .binary_search_by_key(&source, |(source, _)| *source)
                    .expect("候选源有序唯一");
                sources[at].1 = stamp;
            }
        }
    }

    pub(super) fn invalidate_input_epoch(&mut self) {
        self.input_epoch = self.input_epoch.and_then(|epoch| epoch.checked_add(1));
    }
    pub(super) fn refresh_tint_index(&mut self, handle: Handle) {
        let Some(value) = self.state.chunk(handle) else {
            return;
        };
        let key = key(value);
        let mut tinted = FragmentMask::empty();
        let mut custom = FragmentMask::empty();
        for resource in [self.state.resource(handle), self.state.replacement(handle)]
            .into_iter()
            .flatten()
        {
            for bindings in &resource.bindings {
                tinted.set(bindings.fragment).expect("已验证片段身份");
                if bindings.custom {
                    custom.set(bindings.fragment).expect("已验证片段身份");
                }
            }
        }
        if tinted.is_empty() {
            self.tinted.remove(&key);
            self.tinted_spatial.remove(key);
        } else {
            self.tinted.insert(key, tinted);
            self.tinted_spatial.insert(key);
        }
        if custom.is_empty() {
            self.custom_tinted.remove(&key);
        } else {
            self.custom_tinted.insert(key, custom);
        }
    }

    pub(super) fn refresh_ledger(&mut self, handle: Handle) {
        let Some(chunk) = self.state.chunk(handle) else {
            return;
        };
        let current = self.state.resource(handle);
        let next = self.state.replacement(handle).or(current);
        let projected =
            if let Some(job) = self.handle_job_index(handle).map(|index| &self.jobs[index]) {
                if self.compact.generation == 0 && job.unavailable != 0 {
                    current.map_or(0, RegionMesh::len)
                } else {
                    job.projected
                }
            } else if let Some(partial) = self.recovered.get(&key(chunk)) {
                current.map_or(0, |old| old.len() - old.fragment_quads(partial.changed))
                    + partial.parts.len()
            } else {
                next.map_or(0, RegionMesh::len)
            };
        self.ledger.update(
            key(chunk),
            Contribution {
                handle,
                projected,
                excluded: next.is_some_and(|resource| resource.excluded),
                unavailable_sections: next.map_or(0, |resource| {
                    resource.missing_sections.count_ones() as usize
                }),
            },
        );
    }

    pub(super) fn refresh_job_projection(&mut self, index: usize) {
        let job = &mut self.jobs[index];
        let replaced = change_runtime::subtract(job.changed, fragments(job.unavailable));
        job.projected = self
            .state
            .resource(job.handle)
            .map_or(0, |old| old.len() - old.fragment_quads(replaced))
            + job
                .sections
                .iter()
                .map(|(_, mesh)| mesh.quads.len())
                .sum::<usize>();
        let handle = job.handle;
        self.refresh_ledger(handle);
    }

    pub(super) fn grow(&mut self) -> Result<(), Error> {
        let current = self.state.config().capacity();
        let capacity = current.checked_mul(2).ok_or(Error::Input("节点容量耗尽"))?;
        self.state.grow_capacity(capacity)?;
        self.planner_pending = true;
        Ok(())
    }

    pub(super) fn begin_work_turn(&mut self) {
        self.total_progress = self.total_progress.combined(self.progress);
        self.progress = WorkProgress::default();
        self.turn = self.turn.saturating_add(1);
        self.mesh_remaining = self.work_budget.mesh_slabs;
        self.ready_remaining = self.work_budget.maintenance;
        self.retirement_remaining = self.work_budget.maintenance;
        self.tint_remaining = self.work_budget.maintenance;
        self.publication_remaining = self.work_budget.publications;
        self.ready_used = 0;
        self.tint_used = 0;
        self.retirement_used = 0;
    }

    /// 确定性推进额度；零值只暂停对应域，调用不重置当前轮已使用的额度。
    pub fn set_work_budget(&mut self, budget: WorkBudget) {
        self.work_budget = budget;
        self.mesh_remaining = budget.mesh_slabs.saturating_sub(self.progress.mesh_slabs);
        self.publication_remaining = budget
            .publications
            .saturating_sub(self.progress.publications);
        self.ready_remaining = budget.maintenance.saturating_sub(self.ready_used);
        self.tint_remaining = budget.maintenance.saturating_sub(self.tint_used);
        self.retirement_remaining = budget.maintenance.saturating_sub(self.retirement_used);
    }

    /// 当前配置；O(1)，不遍历任务或来源。
    pub const fn work_budget(&self) -> WorkBudget {
        self.work_budget
    }

    /// 本轮与会话累计吞吐；读取不扫描State或来源缓存。
    pub fn work_stats(&self) -> WorkStats {
        WorkStats {
            turn: self.turn,
            current: self.progress,
            total: self.total_progress.combined(self.progress),
            queued_slabs: self.compact.ready_len(),
            unavailable_sections: self.ledger.unavailable_sections(),
            inflight_jobs: self.jobs.len(),
            pending_captures: self.captures.len(),
        }
    }

    /// 独占payload、源页与State容量快照；不遍历资源或来源，不包含线程栈和GPU。
    pub fn payload_stats(&self) -> crate::PayloadStats {
        let source = self.compact.store.stats();
        crate::PayloadStats {
            inflight_jobs: self.jobs.len(),
            pending_captures: self.captures.len(),
            owned_quads: self.ledger.owned,
            projected_quads: self.ledger.projected(),
            source_bytes: source.raw_bytes,
            source_pages: source.pages,
            source_sections: source.sources,
            dependency_edges: source.dependency_edges,
            state_capacity: self.state.config().capacity(),
            occupied_nodes: self.state.len(),
            state_reserved_bytes: self.state.reserved_bytes(),
        }
    }
}
