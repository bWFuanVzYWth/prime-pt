//! 区域任务的容量准入与完成；已拥有片段在候选发布前保留。

use super::*;

impl Scene {
    /// 返回当前CPU逻辑payload限制；不包含GPU、源缓存、线程栈或分配器开销。
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// 更新限制并分步重试预算排除资源；非法配置保留旧限制与资源。
    pub fn set_budget(&mut self, budget: Budget) -> Result<(), Error> {
        let stride = size_of::<prime_scene::Quad>();
        if budget.resident_bytes < stride
            || budget.staging_bytes < stride
            || budget.region_bytes < stride
            || budget.region_bytes > budget.staging_bytes
            || budget.region_bytes > budget.resident_bytes
            || budget
                .resident_bytes
                .checked_add(budget.staging_bytes)
                .is_none()
        {
            return Err(Error::Input("CPU payload预算域无效"));
        }
        if self.budget == budget {
            return Ok(());
        }
        let required = if self.compact.generation == 0 {
            self.estimated_resident(&[])
        } else {
            self.ledger.resident()
        }
        .saturating_mul(size_of::<prime_scene::Quad>());
        self.budget = budget;
        self.pressure = Pressure::default();
        if required > budget.resident_bytes {
            self.invalidate_tints();
            self.pressure = Pressure {
                kind: PressureKind::Resident,
                required_bytes: required,
                limit_bytes: budget.resident_bytes,
                ..Pressure::default()
            };
        }
        self.blocked_quads = 0;
        self.retry_handles.clear();
        self.retry_handles.extend(
            self.ledger
                .excluded_handles()
                .filter(|&handle| !self.capacity.has_wait(handle)),
        );
        self.retry_excluded = (!self.retry_handles.is_empty()).then_some(0);
        self.planner_pending = true;
        Ok(())
    }

    /// 当前压力与被排除数量；BudgetExcluded不当作缺失源或真实空资源。
    pub fn pressure(&self) -> Pressure {
        let mut value = self.pressure;
        value.excluded_regions = self.ledger.excluded();
        if value.excluded_regions != 0 && value.kind != PressureKind::Transition {
            value.kind = self.exclusion.kind;
            value.required_bytes = self.exclusion.required_bytes;
            value.limit_bytes = self.exclusion.limit_bytes;
        }
        value.deferred_requests = if value.kind == PressureKind::Transition {
            self.captures.len()
        } else {
            0
        };
        let required = if self.compact.generation == 0 {
            self.ledger.projected()
        } else {
            self.ledger.resident()
        }
        .saturating_mul(size_of::<prime_scene::Quad>());
        if required > self.budget.resident_bytes && value.kind != PressureKind::Transition {
            value.kind = PressureKind::Resident;
            value.required_bytes = required;
            value.limit_bytes = self.budget.resident_bytes;
        } else if value.excluded_regions == 0 && value.kind != PressureKind::Transition {
            value.kind = PressureKind::None;
        }
        value
    }

    pub(super) fn exclude_job(
        &mut self,
        index: usize,
        kind: PressureKind,
        required: usize,
        limit: usize,
    ) -> Result<(), Error> {
        let revision = self.next_revision;
        let content_revision = self.next_content;
        let next = revision
            .checked_add(1)
            .ok_or(Error::Input("几何revision溢出"))?;
        let next_content = content_revision
            .checked_add(1)
            .ok_or(Error::Input("内容revision溢出"))?;
        let job = self.drop_job(index);
        let old = self.state.resource(job.handle);
        let resource = RegionMesh::candidate(
            job.region,
            old,
            MeshParts::default(),
            FragmentMask::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            old.map_or(revision, |old| old.revision),
            old.map_or(content_revision, |old| old.content_revision),
            old.map_or(0, |old| old.missing_sections),
            true,
        )?;
        let _ = self.state.complete(job.ticket, resource);
        self.refresh_ledger(job.handle);
        self.next_revision = next;
        self.next_content = next_content;
        self.pressure = Pressure {
            kind,
            required_bytes: required,
            limit_bytes: limit,
            ..Pressure::default()
        };
        self.exclusion = self.pressure;
        self.planner_pending = true;
        Ok(())
    }

    pub(super) fn estimate_job(
        &self,
        job: &RegionJob,
        sources: &[(&Section<'_>, Ticket, usize)],
    ) -> usize {
        if job.unavailable != 0
            || sources
                .iter()
                .any(|(source, ticket, _)| *ticket == job.ticket && !source.available)
        {
            return self.state.resource(job.handle).map_or(0, RegionMesh::len);
        }
        job.projected
            + sources
                .iter()
                .filter(|(_, ticket, _)| *ticket == job.ticket)
                .map(|(source, _, _)| source.quads)
                .sum::<usize>()
    }

    pub(super) fn estimated_resident(&self, sources: &[(&Section<'_>, Ticket, usize)]) -> usize {
        let bounds = self.state.config().bounds().expect("有界目标");
        let mut total = self.ledger.projected();
        let mut seen = std::collections::HashSet::new();
        for &(_, ticket, _) in sources {
            if !seen.insert(ticket) {
                continue;
            }
            let Some(index) = self.job_index(ticket) else {
                continue;
            };
            let job = &self.jobs[index];
            if bounds.intersects(chunk(job.region)) {
                total = total
                    - self
                        .ledger
                        .contribution(job.region)
                        .map_or(0, |entry| entry.projected)
                    + self.estimate_job(job, sources);
            }
        }
        total
    }

    pub(super) fn admit_sources(
        &mut self,
        sources: &mut Vec<(&Section<'_>, Ticket, usize)>,
    ) -> Result<bool, Error> {
        let stride = size_of::<prime_scene::Quad>();
        let mut index = 0;
        while index < self.jobs.len() {
            let required = self.estimate_job(&self.jobs[index], sources) * stride;
            if required > self.budget.region_bytes {
                let ticket = self.jobs[index].ticket;
                self.exclude_job(
                    index,
                    PressureKind::Region,
                    required,
                    self.budget.region_bytes,
                )?;
                sources.retain(|(_, value, _)| *value != ticket);
            } else {
                index += 1;
            }
        }
        let mut resident = self.estimated_resident(sources) * stride;
        let mut index = self.jobs.len();
        while resident > self.budget.resident_bytes && index > 0 {
            index -= 1;
            let ticket = self.jobs[index].ticket;
            if !sources.iter().any(|(_, value, _)| *value == ticket)
                && self.jobs[index].sections.is_empty()
            {
                continue;
            }
            if self.estimate_job(&self.jobs[index], sources) == 0 {
                continue;
            }
            self.exclude_job(
                index,
                PressureKind::Resident,
                resident,
                self.budget.resident_bytes,
            )?;
            sources.retain(|(_, value, _)| *value != ticket);
            resident = self.estimated_resident(sources) * stride;
        }
        let limit = (self.budget.resident_bytes + self.budget.staging_bytes) / stride;
        let incoming = |sources: &[(&Section<'_>, Ticket, usize)]| {
            sources
                .iter()
                .filter(|(_, ticket, _)| {
                    !sources
                        .iter()
                        .any(|(source, value, _)| *value == *ticket && !source.available)
                })
                .map(|(source, _, _)| source.quads)
                .sum::<usize>()
        };
        // 先丢弃较远的部分job；固定保留队首一个，保证有效单region可以取得过渡余量。
        while self.ledger.owned + incoming(sources) > limit && self.jobs.len() > 1 {
            let job = self.drop_job(self.jobs.len() - 1);
            let _ = self.state.fail(job.ticket);
            sources.retain(|(_, ticket, _)| *ticket != job.ticket);
        }
        // 取消较远job会恢复其旧可见资源，必须重新评估最终常驻量，不能误判为可等待的过渡。
        let mut resident = self.estimated_resident(sources) * stride;
        let mut index = self.jobs.len();
        while resident > self.budget.resident_bytes && index > 0 {
            index -= 1;
            let ticket = self.jobs[index].ticket;
            if self.estimate_job(&self.jobs[index], sources) == 0
                || !sources.iter().any(|(_, value, _)| *value == ticket)
                    && self.jobs[index].sections.is_empty()
            {
                continue;
            }
            self.exclude_job(
                index,
                PressureKind::Resident,
                resident,
                self.budget.resident_bytes,
            )?;
            sources.retain(|(_, value, _)| *value != ticket);
            resident = self.estimated_resident(sources) * stride;
        }
        if self.ledger.owned + incoming(sources) > limit {
            self.blocked_quads = incoming(sources);
            self.pressure = Pressure {
                kind: PressureKind::Transition,
                required_bytes: (self.ledger.owned + self.blocked_quads) * stride,
                limit_bytes: limit * stride,
                ..Pressure::default()
            };
            return Ok(false);
        }
        Ok(true)
    }

    pub(super) fn finish_jobs(&mut self) -> Result<(), Error> {
        let count = self.completion_jobs.len();
        let first_revision = self.next_revision;
        let first_content = self.next_content;
        let next = self
            .next_revision
            .checked_add(count as u64)
            .ok_or(Error::Input("region revision溢出"))?;
        let next_content = self
            .next_content
            .checked_add(count as u64)
            .ok_or(Error::Input("内容revision溢出"))?;
        // 先保留整个序号域；后续任一候选失败也不能复用已经完成资源的revision。
        self.next_revision = next;
        self.next_content = next_content;
        let mut offset = 0;
        while let Some((_, ticket)) = self.completion_jobs.pop_first() {
            let Some(index) = self.job_index(ticket) else {
                continue;
            };
            let job = &self.jobs[index];
            let legacy_unavailable = self.compact.generation == 0 && job.unavailable != 0;
            if job.invalidated
                || Some(job.epoch) != self.input_epoch
                || (!legacy_unavailable
                    && ((job.received | job.unavailable) != job.dirty
                        || !job.tint_mask.is_empty()
                        || job.pending_tints != 0))
            {
                continue;
            }
            if self.compact.generation != 0 && !self.admit_compact_candidate(index)? {
                continue;
            }
            let job = self.remove_job(index);
            let source_stamps = self.compact.job_sources(job.ticket);
            self.compact.forget_job(job.ticket, job.pending_tints != 0);
            self.release_captures(job.ticket);
            let partial = job
                .sections
                .iter()
                .map(|(_, mesh)| mesh.quads.len())
                .sum::<usize>();
            let build = (|| -> Result<RegionMesh, Error> {
                let mut parts = MeshParts::default();
                if !legacy_unavailable {
                    for (fragment, mesh) in job.sections {
                        parts.insert(fragment, mesh)?;
                    }
                }
                let old = self.state.resource(job.handle);
                let changed = if legacy_unavailable {
                    FragmentMask::empty()
                } else {
                    change_runtime::subtract(job.changed, fragments(job.unavailable))
                };
                let (bindings, patches, colors) = if legacy_unavailable {
                    (Vec::new(), Vec::new(), Vec::new())
                } else {
                    (job.bindings, job.tint_patches, job.color_updates)
                };
                let missing_sections = if legacy_unavailable {
                    u64::MAX
                } else {
                    (old.map_or(0, |old| old.missing_sections) & !job.dirty) | job.unavailable
                };
                RegionMesh::candidate(
                    job.region,
                    old,
                    parts,
                    changed,
                    bindings,
                    patches,
                    colors,
                    first_revision + offset,
                    first_content + offset,
                    missing_sections,
                    false,
                )
            })();
            let mut resource = match build {
                Ok(resource) => resource,
                Err(error) => {
                    self.ledger.owned -= partial;
                    self.state.fail(job.ticket)?;
                    self.mark_fragments(
                        job.handle,
                        job.changed,
                        self.tinted.get(&job.region).copied().unwrap_or_default(),
                    );
                    self.state.invalidate(job.handle)?;
                    self.refresh_ledger(job.handle);
                    self.planner_pending = true;
                    return Err(error);
                }
            };
            let old = self.state.resource(job.handle);
            if let Some(old) = old {
                let (same_geometry, same_content) = resource.same_output(old);
                if same_geometry {
                    resource.revision = old.revision;
                }
                if same_content {
                    resource.content_revision = old.content_revision;
                }
            }
            if legacy_unavailable {
                self.ledger.owned -= partial;
            }
            if let Err(rejected) = self.state.complete(job.ticket, resource) {
                self.ledger.owned -= rejected.resource.owned_quads();
            } else {
                self.install_candidate_sources(job.region, source_stamps);
                self.candidate_epochs
                    .insert(job.region, (job.epoch, job.interactive));
            }
            self.refresh_ledger(job.handle);
            self.refresh_tint_index(job.handle);
            if job.interactive
                && self.state.replacement(job.handle).is_some()
                && self.pending.len() < self.publication_remaining.min(SWITCHES)
                && self
                    .state
                    .config()
                    .bounds()
                    .expect("有界目标")
                    .intersects(chunk(job.region))
            {
                // 可见替换已完成，直接占用本轮剩余发布额度；仍由prepare校验并由GPU确认。
                match self.state.prepare_replace(job.handle) {
                    Ok(patch) => self.pending.push(patch),
                    Err(cm::StateError::RetirementFull) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            offset += 1;
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    /// 测量夹具的收敛条件；预算排除是已解析结果，通过pressure另行报告。
    pub fn pending(&self) -> bool {
        self.dirty
            || self.target.is_none()
            || !self.queued_changes.is_empty()
            || !self.edit_sources.is_empty()
            || !self.edit_refreshes.is_empty()
            || self.resident_retry_ready()
            || self.tint_sweep.pending()
            || self.custom_sweep.pending()
            || self.planner_pending
            || self.scan.is_some()
            || self.retry_excluded.is_some()
            || !self.captures.is_empty()
            || !self.jobs.is_empty()
            || !self.pending.is_empty()
            || !self.parked.is_empty()
            || !self.removes.is_empty()
            || self.state.retired_len() != 0
            || self.state.pending_requests() != 0
            || self.state.pending_patches() != 0
    }
    #[cfg(any(test, feature = "test-fixtures"))]
    /// 测试夹具将当前借用视图复制为连续mesh；生产路径不调用。
    pub fn merge(&self) -> Mesh {
        let mut result = Mesh::default();
        for (key, _, mesh) in self.entries() {
            let offset = std::array::from_fn(|axis| {
                (f64::from(key[axis]) * 64.0 - self.anchor[axis]) as f32
            });
            result.quads.extend(mesh.quads().map(|quad| {
                let mut quad = *quad;
                quad.translate(offset);
                quad
            }));
        }
        result
    }
}
