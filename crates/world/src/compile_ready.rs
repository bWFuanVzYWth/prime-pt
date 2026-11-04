//! 中心证明、反向依赖唤醒与有界批量内核；邻域按section建立一次，slab输出直接归job。
use super::*;
use prime_minecraft::{CompiledSlab, SlabCompile};

impl Scene {
    pub(super) fn advance_sources(&mut self) -> Result<(), Error> {
        let woken: Vec<_> = self
            .compact
            .store
            .drain_woken(self.ready_remaining)
            .collect();
        for work in woken {
            if let Some(index) = self
                .compact
                .groups
                .get(&work)
                .and_then(|&ticket| self.job_index(ticket))
            {
                self.compact
                    .todo
                    .insert(work, self.jobs[index].interactive)?;
            }
        }
        while self.ready_remaining != 0 {
            let Some(work) = self.compact.todo.pop_first() else {
                break;
            };
            self.ready_remaining -= 1;
            self.ready_used += 1;
            let Some(&ticket) = self.compact.groups.get(&work) else {
                continue;
            };
            let Some(index) = self.job_index(ticket) else {
                continue;
            };
            if self.jobs[index].invalidated || Some(self.jobs[index].epoch) != self.input_epoch {
                for offset in 0..4 {
                    self.compact.ready.remove(&WorkId {
                        fragment: work.fragment + offset,
                        ..work
                    });
                }
                continue;
            }
            let slot = usize::from(work.fragment) / 4;
            if self.jobs[index].unavailable & (1 << slot) != 0 {
                continue;
            }
            let center = section(work.region, slot);
            let wanted = (slot * 4..slot * 4 + 4)
                .filter(|&fragment| {
                    self.jobs[index].changed.contains(fragment)
                        && !self.jobs[index].finished.contains(fragment)
                })
                .count();
            match self.compact.store.lookup(center) {
                SourceState::Unknown => {
                    self.progress.waiting_sources += wanted;
                    continue;
                }
                SourceState::Unavailable => {
                    self.defer_section(index, work, center)?;
                    continue;
                }
                SourceState::Air => {
                    if self.compact.expanded.remove(&work) {
                        self.compact.store.depend(work, &[center])?;
                    }
                    let job = &mut self.jobs[index];
                    let mut proved = false;
                    for fragment in slot * 4..slot * 4 + 4 {
                        if job.changed.contains(fragment)
                            && (!job.compiled.contains(fragment) || job.verify.contains(fragment))
                        {
                            if job.verify.contains(fragment) {
                                self.progress.reused_slabs += 1;
                            } else {
                                proved = true;
                            }
                            job.compiled.set(fragment)?;
                            finish_fragment(job, fragment)?;
                        }
                    }
                    self.progress.empty_sections += usize::from(proved);
                    if !job.tint_mask.is_empty() {
                        self.queue_tints(ticket)?;
                    }
                    self.queue_completion(ticket);
                    continue;
                }
                SourceState::Available(_) => {}
            }
            let neighborhood = self.neighborhood(center, self.compact.generation);
            if let Some(missing) = neighborhood
                .sections
                .iter()
                .position(|neighbor| matches!(neighbor, Neighbor::Unavailable))
            {
                let offset = [missing % 3, missing / 9, missing / 3 % 3];
                self.defer_section(
                    index,
                    work,
                    std::array::from_fn(|axis| center[axis] + offset[axis] as i32 - 1),
                )?;
                continue;
            }
            let waiting = neighborhood
                .sections
                .iter()
                .any(|neighbor| matches!(neighbor, Neighbor::Unknown));
            if !self.compact.expanded.contains(&work) {
                let mut keys = Vec::with_capacity(27);
                for y in -1..=1 {
                    for z in -1..=1 {
                        for x in -1..=1 {
                            let key = [center[0] + x, center[1] + y, center[2] + z];
                            if key[1] >= self.height[0] && key[1] < self.height[1] {
                                keys.push(key);
                            }
                        }
                    }
                }
                self.compact.store.depend(work, &keys)?;
                self.compact.expanded.insert(work);
            }
            if waiting {
                self.progress.waiting_sources += wanted;
                continue;
            }
            self.progress.source_hits += 1;
            let job = &mut self.jobs[index];
            for fragment in slot * 4..slot * 4 + 4 {
                if job.verify.contains(fragment) {
                    job.compiled.set(fragment)?;
                    finish_fragment(job, fragment)?;
                    self.progress.reused_slabs += 1;
                    continue;
                }
                if job.changed.contains(fragment)
                    && !job.compiled.contains(fragment)
                    && self.compact.ready.insert(
                        WorkId {
                            fragment: fragment as u16,
                            ..work
                        },
                        job.interactive,
                    )?
                {
                    self.progress.ready_slabs += 1;
                }
            }
            self.queue_completion(ticket);
        }
        Ok(())
    }

    fn defer_section(
        &mut self,
        index: usize,
        work: WorkId,
        missing: [i32; 3],
    ) -> Result<(), Error> {
        // 已知未加载的来源足以阻止该section提交，暂不保留其余halo的捕获需求。
        self.compact.store.depend(work, &[missing])?;
        self.compact.expanded.remove(&work);
        let job = &mut self.jobs[index];
        let slot = 1 << (usize::from(work.fragment) / 4);
        let mask = fragments(slot);
        let ticket = job.ticket;
        let handle = job.handle;
        let retained = self.state.resource(handle).map_or(0, |old| {
            let replaced = FragmentMask::from_words(std::array::from_fn(|word| {
                job.changed.words()[word] & mask.words()[word]
            }));
            old.fragment_quads(replaced)
        });
        self.compact.cancel_fragments(job, mask)?;
        self.compact.todo.remove(&work);
        let mut discarded = 0;
        job.sections.retain(|(fragment, mesh)| {
            if mask.contains(*fragment) {
                discarded += mesh.quads.len();
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
        job.tint_mask = change_runtime::subtract(job.tint_mask, mask);
        job.compiled = change_runtime::subtract(job.compiled, mask);
        job.verify = change_runtime::subtract(job.verify, mask);
        job.finished = change_runtime::subtract(job.finished, mask);
        job.received &= !slot;
        job.unavailable |= slot;
        self.ledger.owned -= discarded;
        job.projected = job.projected - discarded + retained;
        self.refresh_ledger(handle);
        self.queue_completion(ticket);
        Ok(())
    }

    pub(super) fn compile_ready(&mut self, catalog: &Catalog) -> Result<(), Error> {
        let stride = size_of::<prime_scene::Quad>();
        self.progress.waiting_budget += self.compact.capacity.len();
        while self.mesh_remaining != 0 {
            let common = self.staging_available(false);
            let full = self.staging_available(true);
            if full == 0 {
                self.progress.waiting_budget += self.compact.ready.len();
                break;
            }
            let mut owner = self.capacity.staging_owner();
            self.wake_capacity(false, common)?;
            self.wake_capacity(true, if owner.is_none() { full } else { common })?;
            let mut reserved = Vec::new();
            if let Some(ticket) = owner {
                self.collect_reserved_work(ticket, full, &mut reserved)?;
                if reserved.is_empty()
                    || full == common
                    || common >= self.budget.region_bytes / stride
                {
                    self.capacity.release_staging(ticket);
                    reserved.clear();
                    owner = None;
                    self.wake_capacity(true, full)?;
                }
            }
            let reserved_ticket = if full > common && common < self.budget.region_bytes / stride {
                owner.or_else(|| {
                    self.compact.ready.lane(true).next().and_then(|work| {
                        self.compact
                            .groups
                            .get(&WorkId {
                                fragment: work.fragment / 4 * 4,
                                ..*work
                            })
                            .copied()
                    })
                })
            } else {
                None
            };
            if let Some(ticket) = reserved_ticket
                && owner.is_none()
            {
                self.collect_reserved_work(ticket, full, &mut reserved)?;
            }
            let Some(interactive) = self
                .compact
                .ready
                .select(common != 0 || !reserved.is_empty(), common != 0)
            else {
                self.progress.waiting_budget += self.compact.ready.len();
                break;
            };
            let uses_reserved = interactive && !reserved.is_empty();
            let available = if uses_reserved { full } else { common };
            let ready_count = if uses_reserved {
                reserved.len()
            } else {
                self.compact.ready.lane(interactive).len()
            };
            let expanded = self.pool.current_num_threads().saturating_mul(8);
            let expanded_count = self.mesh_remaining.min(expanded).min(ready_count);
            // 余量至少容纳每项一个未剔除cube slab时放大批次；这只是调度门槛，
            // 不作为任意模型的输出上界。压力或真实限额失败仍走原小波和minimum收缩。
            let wave_limit = if self.pressure.kind == PressureKind::None
                && self.compact.minimums.is_empty()
                && available / expanded_count >= 16 * 16 * 4 * 6
            {
                expanded
            } else {
                8
            };
            let count = self
                .mesh_remaining
                .min(wave_limit)
                .min(available)
                .min(ready_count);
            let mut work: Vec<_> = if uses_reserved {
                reserved.truncate(count);
                reserved
            } else {
                self.compact
                    .ready
                    .lane(interactive)
                    .take(count)
                    .copied()
                    .collect()
            };
            work.retain(|work| {
                let minimum = self.compact.minimums.get(work).copied().unwrap_or(1);
                if minimum <= available {
                    return true;
                }
                self.compact.ready.remove(work);
                self.compact.capacity.insert(*work, (minimum, interactive));
                self.compact.capacity_index[usize::from(interactive)].insert((minimum, *work));
                false
            });
            if work.is_empty() {
                continue;
            }
            while work.len() > 1
                && work.iter().any(|item| {
                    self.compact.minimums.get(item).copied().unwrap_or(1) > available / work.len()
                })
            {
                work.pop();
            }
            let mut groups = Vec::<(WorkId, Neighborhood<'_>)>::new();
            let mut tasks = Vec::with_capacity(work.len());
            let mut invalid = Vec::new();
            for &ready in &work {
                let group = WorkId {
                    fragment: ready.fragment / 4 * 4,
                    ..ready
                };
                if self
                    .compact
                    .groups
                    .get(&group)
                    .and_then(|ticket| self.job_index(*ticket).map(|index| &self.jobs[index]))
                    .is_none_or(|job| job.invalidated || Some(job.epoch) != self.input_epoch)
                {
                    self.compact.ready.remove(&ready);
                    continue;
                }
                let at = if let Some(at) = groups.iter().position(|(key, _)| *key == group) {
                    at
                } else {
                    if invalid.contains(&group) {
                        continue;
                    }
                    let neighborhood = neighborhood_from(
                        &self.compact.store,
                        self.height,
                        section(group.region, usize::from(group.fragment) / 4),
                        catalog.generation(),
                    );
                    if neighborhood
                        .sections
                        .iter()
                        .any(|value| matches!(value, Neighbor::Unknown | Neighbor::Unavailable))
                    {
                        invalid.push(group);
                        self.compact.todo.insert(group, interactive)?;
                        for offset in 0..4 {
                            self.compact.ready.remove(&WorkId {
                                fragment: group.fragment + offset,
                                ..group
                            });
                        }
                        self.progress.waiting_sources += 1;
                        continue;
                    }
                    let at = groups.len();
                    groups.push((group, neighborhood));
                    at
                };
                tasks.push((ready, at));
            }
            if tasks.is_empty() {
                continue;
            }
            self.compact.ready.served(interactive);
            let quota = available / tasks.len();
            let compile = |ready: WorkId, group: usize, max_quads| {
                prime_minecraft::compile_slab(
                    catalog,
                    &groups[group].1,
                    SlabCompile {
                        first_y: usize::from(ready.fragment % 4) * 4,
                        origin: groups[group].1.key.map(|v| (v.rem_euclid(4) * 16) as f32),
                        max_quads,
                    },
                )
            };
            let mut output: Vec<_> = if tasks.len() == 1 {
                let (ready, group) = tasks[0];
                vec![compile(ready, group, quota)]
            } else {
                self.pool.install(|| {
                    tasks
                        .par_iter()
                        .map(|&(ready, group)| compile(ready, group, quota))
                        .collect()
                })
            };
            let mut executed = tasks.len();
            let mut used = output
                .iter()
                .filter_map(|value| value.as_ref().ok())
                .map(|value| value.mesh.quads.len())
                .sum::<usize>();
            for (index, result) in output.iter_mut().enumerate() {
                if matches!(result, Err(prime_minecraft::Error::OutputLimit { .. }))
                    && executed < self.mesh_remaining
                {
                    let (ready, group) = tasks[index];
                    *result = compile(ready, group, available - used);
                    executed += 1;
                    if let Ok(value) = result {
                        used += value.mesh.quads.len();
                    }
                }
            }
            drop(groups);
            if uses_reserved && used > common {
                self.capacity
                    .claim_staging(reserved_ticket.expect("预留任务属于同一候选"));
            }
            self.mesh_remaining -= executed;
            self.progress.mesh_slabs += executed;
            self.progress.mesh_waves += 1;
            let mut blocked = Vec::new();
            for ((ready, _), result) in tasks.into_iter().zip(output) {
                let group = WorkId {
                    fragment: ready.fragment / 4 * 4,
                    ..ready
                };
                let Some(&ticket) = self.compact.groups.get(&group) else {
                    continue;
                };
                match result {
                    Ok(value) => {
                        self.accept_slab(ticket, usize::from(ready.fragment), value, catalog)?;
                        self.compact.ready.remove(&ready);
                        self.compact.minimums.remove(&ready);
                    }
                    Err(prime_minecraft::Error::OutputLimit { required }) => {
                        let required =
                            required.max(self.compact.minimums.get(&ready).copied().unwrap_or(1));
                        self.compact.ready.remove(&ready);
                        self.compact.capacity.insert(ready, (required, interactive));
                        self.compact.capacity_index[usize::from(interactive)]
                            .insert((required, ready));
                        self.compact.minimums.insert(ready, required);
                        blocked.push((ticket, required));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some((ticket, required)) = blocked.first().copied()
                && let Some(index) = self.job_index(ticket)
            {
                let required_region = self.jobs[index].projected.saturating_add(required) * stride;
                self.progress.waiting_budget += blocked.len();
                if required_region > self.budget.region_bytes {
                    self.exclude_job(
                        index,
                        PressureKind::Region,
                        required_region,
                        self.budget.region_bytes,
                    )?;
                } else {
                    self.blocked_quads = required;
                    self.pressure = Pressure {
                        kind: PressureKind::Transition,
                        required_bytes: self.ledger.owned.saturating_add(required) * stride,
                        limit_bytes: self.staging_limit(uses_reserved),
                        ..Pressure::default()
                    };
                }
            }
        }
        Ok(())
    }

    fn wake_capacity(&mut self, interactive: bool, available: usize) -> Result<(), Error> {
        let index = &mut self.compact.capacity_index[usize::from(interactive)];
        let mut count = self
            .mesh_remaining
            .saturating_sub(self.compact.ready.lane(interactive).len());
        while count != 0
            && let Some(&(required, work)) = index.first()
        {
            if required > available {
                break;
            }
            self.compact.ready.insert(work, interactive)?;
            index.pop_first();
            self.compact.capacity.remove(&work);
            count -= 1;
        }
        Ok(())
    }

    fn collect_reserved_work(
        &mut self,
        ticket: Ticket,
        available: usize,
        work: &mut Vec<WorkId>,
    ) -> Result<(), Error> {
        let Some(groups) = self.compact.registrations.get(&ticket) else {
            return Ok(());
        };
        for &group in groups.values() {
            for offset in 0..4 {
                let item = WorkId {
                    fragment: group.fragment + offset,
                    ..group
                };
                if let Some((required, true)) = self.compact.capacity.get(&item).copied()
                    && required <= available
                {
                    self.compact.ready.insert(item, true)?;
                    self.compact.capacity.remove(&item);
                    self.compact.capacity_index[1].remove(&(required, item));
                }
                if self.compact.ready.contains(&item, true) {
                    work.push(item);
                }
            }
        }
        Ok(())
    }

    fn accept_slab(
        &mut self,
        ticket: Ticket,
        fragment: usize,
        output: CompiledSlab,
        catalog: &Catalog,
    ) -> Result<(), Error> {
        let Some(index) = self.job_index(ticket) else {
            return Ok(());
        };
        let key = section(self.jobs[index].region, fragment / 4);
        let mut patches = Vec::new();
        patches.try_reserve_exact(output.bindings.len())?;
        let mut custom = false;
        for (span, &binding) in output.bindings.iter().enumerate() {
            custom |= catalog.tint_is_custom(binding.source)?;
            let request = catalog.tint_request(self.compact.generation, key, binding)?;
            patches.push((span, self.compact.query(request)));
        }
        let job = &mut self.jobs[index];
        self.ledger.owned += output.mesh.quads.len();
        job.projected += output.mesh.quads.len();
        if !output.mesh.quads.is_empty() {
            job.sections.push((fragment, output.mesh));
        }
        job.compiled.set(fragment)?;
        if output.bindings.is_empty() {
            finish_fragment(job, fragment)?;
        } else {
            let colors = vec![0; output.bindings.len()];
            job.bindings.push(FragmentBindings {
                fragment,
                spans: output.bindings,
                colors,
                custom,
            });
            self.compact.tints.push(PendingTint {
                ticket,
                fragment,
                old: false,
                patches,
            });
            job.pending_tints += 1;
            job.pending_tint_mask.set(fragment)?;
        }
        let mut tint_mask = job.tint_mask.words();
        tint_mask[fragment / 64] &= !(1 << (fragment % 64));
        job.tint_mask = FragmentMask::from_words(tint_mask);
        let tint_pending = !job.tint_mask.is_empty();
        let handle = job.handle;
        self.refresh_ledger(handle);
        if tint_pending {
            self.queue_tints(ticket)?;
        }
        self.queue_completion(ticket);
        Ok(())
    }
}
