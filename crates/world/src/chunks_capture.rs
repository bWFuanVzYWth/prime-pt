//! 来源捕获与请求续传；缺失和失败不撤销已发布的覆盖。

use super::*;

impl Scene {
    pub(super) fn release_captures(&mut self, ticket: Ticket) {
        let mut index = 0;
        while index < self.captures.len() {
            if self.captures[index].ticket != Some(ticket) {
                index += 1;
                continue;
            }
            let capture = self.captures[index].wire;
            let other = if self.compact.generation != 0 {
                self.compact.capture_owner(capture.section)
            } else {
                None
            };
            if let Some(ticket) = other {
                self.captures[index].ticket = Some(ticket);
                index += 1;
            } else {
                self.captures.remove(index);
            }
        }
    }

    pub(super) fn begin_region_job(&mut self, build: cm::Build) -> Result<(), Error> {
        let handle = self.state.find(build.chunk).expect("已签发region保持节点");
        self.edit_refreshes.remove(&handle);
        let mask = self.masks.remove(&key(build.chunk)).unwrap_or_default();
        let recovered = self.recovered.remove(&key(build.chunk));
        let old = self.state.resource(handle);
        let mut changed = if mask.handle == Some(handle)
            && (!mask.mask.is_empty() || !mask.tints.is_empty())
            && (old.is_some() || recovered.is_some())
        {
            mask.mask
        } else {
            FragmentMask::all()
        };
        if !changed.is_empty()
            && let Some(old) = old
        {
            if old.excluded || self.compact.generation == 0 && old.missing_sections != 0 {
                changed = FragmentMask::all();
            } else {
                changed = change_runtime::union(
                    changed,
                    fragments(old.missing_sections & fragment_sections(changed)),
                );
            }
        }
        let mut sections = Vec::new();
        let mut bindings = Vec::new();
        let mut color_updates = Vec::new();
        let mut verify = FragmentMask::empty();
        let mut tint_patches = Vec::new();
        if let Some(recovered) = recovered {
            // 缺源时没有生成新几何，恢复候选也须重新验证这些section。
            changed = change_runtime::union(changed, fragments(recovered.missing_sections));
            // 未受影响的未发布片段只转移所有权，依赖重新齐备后复用，不复制Quad。
            verify = change_runtime::subtract(recovered.changed, changed);
            for (slot, mesh) in recovered.parts {
                if changed.contains(slot) {
                    self.ledger.owned -= mesh.quads.len();
                } else {
                    sections.push((slot, mesh));
                }
            }
            bindings = recovered.bindings;
            bindings.retain(|entry| !changed.contains(entry.fragment));
            color_updates = recovered.color_updates;
            // 重染片段用下一次回填建立补丁，不能同时继承上一候选的颜色差分。
            let recolored = change_runtime::union(changed, mask.tints);
            color_updates.retain(|entry| !recolored.contains(entry.fragment));
            for group in bindings
                .iter()
                .filter(|group| !mask.tints.contains(group.fragment))
            {
                for (span, &color) in group.spans.iter().zip(&group.colors) {
                    tint_patches.push(prime_scene::TintPatch::new(
                        group.fragment,
                        span.first as usize,
                        span.count as usize,
                        color,
                    )?);
                }
            }
            if let Some(old) = self.state.resource(handle) {
                for update in &color_updates {
                    let group = old
                        .bindings
                        .iter()
                        .find(|entry| entry.fragment == update.fragment)
                        .expect("原候选已校验颜色绑定");
                    let span = group.spans[update.span];
                    tint_patches.push(prime_scene::TintPatch::new(
                        update.fragment,
                        span.first as usize,
                        span.count as usize,
                        update.argb,
                    )?);
                }
            }
            changed = change_runtime::union(changed, verify);
        }
        let dirty = fragment_sections(changed);
        let mut job = RegionJob {
            invalidated: false,
            interactive: mask.handle == Some(handle) && mask.interactive,
            epoch: self
                .input_epoch
                .ok_or(Error::Input("输入世代耗尽，需重建Scene所有者"))?,
            ticket: build.ticket,
            handle,
            region: key(build.chunk),
            dirty,
            issued: 0,
            received: 0,
            unavailable: 0,
            projected: self
                .state
                .resource(handle)
                .map_or(0, |old| old.len() - old.fragment_quads(changed))
                + sections
                    .iter()
                    .map(|(_, mesh)| mesh.quads.len())
                    .sum::<usize>(),
            sections,
            changed,
            compiled: verify,
            verify,
            finished: FragmentMask::empty(),
            tint_mask: mask.tints,
            bindings,
            tint_patches,
            color_updates,
            pending_tints: 0,
            pending_tint_mask: FragmentMask::empty(),
        };
        for slot in 0..64 {
            let y = section(job.region, slot)[1];
            if y < self.height[0] || y >= self.height[1] {
                job.received |= (1 << slot) & dirty;
                for fragment in slot * 4..slot * 4 + 4 {
                    if changed.contains(fragment) {
                        job.compiled.set(fragment)?;
                        job.finished.set(fragment)?;
                    }
                }
            }
        }
        self.insert_job(job)?;
        self.refresh_ledger(handle);
        Ok(())
    }

    /// 视距单位为16m水平列；Y为半开世界section域。返回仍待宿主捕获的请求前缀。
    ///
    /// 分帧回复共享region ticket；每轮至多8个新region，停车任务由依赖事件唤醒。
    /// 近处实际编辑可先使用部分签发额度，持续竞争时给普通任务保留推进机会。
    /// 输入超域保留旧状态；max_requests最多64且同时在途最多64，未完成请求可延后回复。
    pub fn plan(
        &mut self,
        camera: &WorldCamera,
        view_distance: u32,
        min_section_y: i32,
        max_section_y_exclusive: i32,
        max_requests: usize,
    ) -> Result<&[CaptureRequest], Error> {
        self.input_epoch
            .ok_or(Error::Input("输入世代耗尽，需重建Scene所有者"))?;
        self.begin_work_turn();
        let target = self.select(
            camera,
            view_distance,
            min_section_y,
            max_section_y_exclusive,
        )?;
        self.dismiss_view()?;
        self.output.clear();
        self.prioritize_edit_sources(camera)?;
        self.process_changes()?;
        self.drain_retired();
        self.wake_capacity_retries()?;
        self.scan_invalidations()?;
        for _ in 0..self.work_budget.maintenance {
            let Some(handle) = self.retry_handles.pop_front() else {
                break;
            };
            if self
                .state
                .resource(handle)
                .is_some_and(|resource| resource.excluded)
            {
                self.invalidate_handle(handle, u64::MAX)?;
            }
        }
        self.retry_excluded = (!self.retry_handles.is_empty()).then_some(0);
        let owned_limit = self.staging_limit(false) / size_of::<prime_scene::Quad>();
        if self.pressure.kind == PressureKind::Transition
            && self.ledger.owned + self.blocked_quads <= owned_limit
        {
            self.pressure.kind = PressureKind::None;
            self.blocked_quads = 0;
        }
        let mut edit_attempts = 0;
        let mut edit_requests = 0;
        let bounds = self.state.config().bounds().expect("有界目标");
        for _ in 0..(JOBS / 2).min(self.work_budget.planner_visits) {
            let Some(handle) = self.edit_refreshes.pop_first() else {
                break;
            };
            edit_attempts += 1;
            if self.handle_job_index(handle).is_some()
                || !self
                    .state
                    .chunk(handle)
                    .is_some_and(|chunk| bounds.intersects(chunk))
            {
                continue;
            }
            match self.state.request_refresh(handle) {
                Ok(build) => {
                    self.begin_region_job(build)?;
                    edit_requests += 1;
                }
                Err(cm::StateError::PendingPatch | cm::StateError::Transition) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.reserve_publication_retry(bounds)?;
        let hold_edit_slot = self.interactive_jobs != 0 && self.turn % 4 != 0;
        let result = cm::plan(
            &mut self.state,
            target,
            cm::Budget {
                visits: self.work_budget.planner_visits - edit_attempts,
                requests: JOBS - edit_requests,
                switches: self
                    .publication_remaining
                    .min(SWITCHES)
                    .saturating_sub(self.pending.len() + self.parked.len())
                    .saturating_sub(usize::from(hold_edit_slot)),
            },
            &mut self.batch,
        );
        for at in 0..self.batch.cancellations().len() {
            let ticket = self.batch.cancellations()[at];
            if let Some(index) = self.job_index(ticket) {
                self.drop_job(index);
            }
        }
        for request_index in 0..self.batch.requests().len() {
            self.begin_region_job(self.batch.requests()[request_index])?;
        }
        for patch in self.batch.switches() {
            if matches!(patch.change, Change::Remove(_)) {
                self.removes.push(*patch);
            } else {
                self.pending.push(*patch);
            }
        }
        self.batch.clear();
        let stats = result?;
        self.planner_pending = stats.pending;
        if stats.capacity_pressure {
            self.grow()?;
        }
        let count = max_requests.min(REQUESTS);
        let first_wire = self.next_wire;
        let additional = count.saturating_sub(self.captures.len());
        self.next_wire
            .checked_add(additional as u64)
            .ok_or(Error::Input("捕获请求编号溢出"))?;
        let bounds = self.state.config().bounds().expect("有界region域");
        if self.compact.generation == 0 {
            for job in &mut self.jobs {
                if !bounds.intersects(chunk(job.region)) {
                    continue;
                }
                let mut available = job.dirty & !(job.issued | job.received);
                while available != 0 && self.captures.len() < count {
                    let slot = available.trailing_zeros() as usize;
                    let bit = 1 << slot;
                    available &= !bit;
                    job.issued |= bit;
                    let wire = CaptureRequest {
                        id: self.next_wire,
                        section: section(job.region, slot),
                        reserved: 0,
                    };
                    self.next_wire += 1;
                    self.captures.push(PendingCapture {
                        wire,
                        source_stamp: None,
                        ticket: Some(job.ticket),
                        slot,
                    });
                }
            }
        }
        if self.compact.generation != 0 {
            self.issue_compact_captures(count)?;
        }
        self.output
            .extend(self.captures.iter().take(count).map(|capture| capture.wire));
        self.progress.source_requests += (self.next_wire - first_wire) as usize;
        self.finish_jobs()?;
        Ok(&self.output)
    }

    /// 放弃一条宿主捕获；后续以新wire ID重试，不复用旧回复身份。
    pub fn fail_capture(&mut self, id: u64) -> Result<(), Error> {
        if let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.wire.id == id)
        {
            let capture = self.captures.remove(index);
            if let Some(index) = capture.ticket.and_then(|ticket| self.job_index(ticket)) {
                let job = &mut self.jobs[index];
                job.issued &= !(1 << capture.slot);
            }
            self.planner_pending = true;
        }
        Ok(())
    }

    /// 同步消费完整借用源包；协议错误保留旧发布，容量不足用pressure报告。
    pub fn apply(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let sources = crate::decode(bytes)?;
        #[cfg(any(test, feature = "test-fixtures"))]
        if sources.first().is_some_and(|source| source.id == 0) {
            return self.apply_legacy(&sources);
        }
        self.complete_sources(&sources)
    }

    pub(super) fn complete_sources(&mut self, sources: &[Section<'_>]) -> Result<(), Error> {
        let bounds = self.state.config().bounds().expect("有界region域");
        let mut selected = Vec::with_capacity(sources.len());
        for source in sources {
            if let Some(capture) = self
                .captures
                .iter()
                .find(|capture| capture.wire.id == source.id)
            {
                if capture.wire.section != source.key {
                    return Err(Error::Input("捕获id与section坐标不匹配"));
                }
                if bounds.intersects(chunk(region(source.key)))
                    && let Some(ticket) = capture.ticket
                {
                    selected.push((source, ticket, capture.slot));
                }
            }
        }
        let unavailable: Vec<_> = selected
            .iter()
            .filter(|(source, _, _)| !source.available)
            .map(|(_, ticket, _)| *ticket)
            .collect();
        if !self.admit_sources(&mut selected)? {
            return Ok(());
        }
        let incoming: usize = selected
            .iter()
            .filter(|(_, ticket, _)| !unavailable.contains(ticket))
            .map(|(source, _, _)| source.quads)
            .sum();
        let build = |(source, ticket, _): &(&Section<'_>, Ticket, usize)| {
            if unavailable.contains(ticket) {
                return Mesh::default();
            }
            let mut mesh = compile(source);
            let offset = source.key.map(|v| v.rem_euclid(4) as f32 * 16.0);
            if offset != [0.; 3] {
                for quad in &mut mesh.quads {
                    quad.translate(offset);
                }
            }
            mesh
        };
        let meshes: Vec<_> = if selected.len() < 4 || incoming < 1024 {
            selected.iter().map(build).collect()
        } else {
            self.pool
                .install(|| selected.par_iter().map(build).collect())
        };
        for ((source, ticket, slot), mesh) in selected.into_iter().zip(meshes) {
            let mut changed_handle = None;
            if let Some(index) = self.job_index(ticket) {
                let job = &mut self.jobs[index];
                self.ledger.owned += mesh.quads.len();
                job.projected += mesh.quads.len();
                job.sections.push((slot * 4, mesh));
                for fragment in slot * 4..slot * 4 + 4 {
                    job.compiled.set(fragment)?;
                    job.finished.set(fragment)?;
                }
                job.received |= 1 << slot;
                if !source.available {
                    job.unavailable |= 1 << slot;
                }
                changed_handle = Some(job.handle);
            }
            self.queue_completion(ticket);
            if let Some(handle) = changed_handle {
                self.refresh_ledger(handle);
            }
            self.captures.retain(|capture| capture.wire.id != source.id);
        }
        for source in sources {
            if !bounds.intersects(chunk(region(source.key))) {
                self.fail_capture(source.id)?;
            }
        }
        self.finish_jobs()?;
        self.planner_pending = true;
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) fn apply_legacy(&mut self, sources: &[Section<'_>]) -> Result<(), Error> {
        let anchor = self.anchor;
        let origin = sources[0].key.map(|v| f64::from(v) * 16.0);
        let camera = WorldCamera {
            origin,
            forward: [0., 0., 1., 0.],
            right: [1., 0., 0., 0.],
            up: [0., 1., 0., 0.],
        };
        let base_y = sources[0].key[1].div_euclid(4) * 4;
        for source in sources {
            self.invalidate_section(source.key)?;
        }
        let mut remaining = vec![true; sources.len()];
        for _ in 0..CAPACITY * 16 {
            let requests = self
                .plan(&camera, 0, base_y, base_y + 4, REQUESTS)?
                .to_vec();
            for request in requests {
                if let Some(index) = sources
                    .iter()
                    .enumerate()
                    .position(|(index, source)| remaining[index] && source.key == request.section)
                {
                    let source = &sources[index];
                    let proxy = Section {
                        id: request.id,
                        key: source.key,
                        available: source.available,
                        blocks: source.blocks.clone(),
                        quads: source.quads,
                    };
                    self.complete_sources(std::slice::from_ref(&proxy))?;
                    remaining[index] = false;
                } else {
                    let source = Section {
                        id: request.id,
                        key: request.section,
                        available: true,
                        blocks: Vec::new(),
                        quads: 0,
                    };
                    self.complete_sources(std::slice::from_ref(&source))?;
                }
            }
            if self.prepare()? {
                self.commit()?;
            }
            let published = sources.iter().all(|source| {
                self.state
                    .find(chunk(region(source.key)))
                    .is_some_and(|handle| {
                        self.state.is_visible(handle) && self.state.replacement(handle).is_none()
                    })
            });
            if remaining.iter().all(|value| !value) && published {
                self.rebase(anchor);
                return Ok(());
            }
        }
        Err(Error::Input("测试PRT1捕获未收敛"))
    }
}
