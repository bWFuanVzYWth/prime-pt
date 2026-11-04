//! 常驻正增长在完整候选边界接纳，过渡空间在内核运行前按实际独占量分配。
use super::*;
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Copy)]
struct WaitingGrowth {
    handle: Handle,
    quads: usize,
    interactive: bool,
    active: bool,
}

#[derive(Default)]
pub(super) struct Capacity {
    waiting: HashMap<usize, WaitingGrowth>,
    ready: [BTreeSet<(usize, usize)>; 2],
    staging_owner: Option<Ticket>,
}

impl Capacity {
    fn park(&mut self, handle: Handle, quads: usize, interactive: bool) {
        self.remove_slot(handle.slot());
        self.waiting.insert(
            handle.slot(),
            WaitingGrowth {
                handle,
                quads,
                interactive,
                active: false,
            },
        );
    }

    pub(super) fn activate_wait(&mut self, handle: Handle) {
        if let Some(wait) = self.waiting.get_mut(&handle.slot())
            && wait.handle == handle
            && !wait.active
        {
            wait.active = true;
            self.ready[usize::from(wait.interactive)].insert((wait.quads, handle.slot()));
        }
    }

    pub(super) fn pause_wait(&mut self, handle: Handle) {
        if let Some(wait) = self.waiting.get_mut(&handle.slot())
            && wait.handle == handle
            && wait.active
        {
            wait.active = false;
            self.ready[usize::from(wait.interactive)].remove(&(wait.quads, handle.slot()));
        }
    }

    pub(super) fn remove_wait(&mut self, handle: Handle) {
        if self.has_wait(handle) {
            self.remove_slot(handle.slot());
        }
    }

    pub(super) fn has_wait(&self, handle: Handle) -> bool {
        self.waiting
            .get(&handle.slot())
            .is_some_and(|wait| wait.handle == handle)
    }

    fn remove_slot(&mut self, slot: usize) {
        if let Some(wait) = self.waiting.remove(&slot)
            && wait.active
        {
            self.ready[usize::from(wait.interactive)].remove(&(wait.quads, slot));
        }
    }

    fn fitting(&self, interactive: bool, available: usize) -> Option<WaitingGrowth> {
        let &(quads, slot) = self.ready[usize::from(interactive)].first()?;
        (quads <= available).then(|| self.waiting[&slot])
    }

    pub(super) const fn staging_owner(&self) -> Option<Ticket> {
        self.staging_owner
    }

    pub(super) fn claim_staging(&mut self, ticket: Ticket) {
        assert!(self.staging_owner.is_none_or(|owner| owner == ticket));
        self.staging_owner = Some(ticket);
    }

    pub(super) fn release_staging(&mut self, ticket: Ticket) {
        if self.staging_owner == Some(ticket) {
            self.staging_owner = None;
        }
    }
}

impl Scene {
    fn resident_limit(&self, interactive: bool) -> usize {
        let resident = self.budget.resident_bytes;
        if interactive {
            return resident;
        }
        let reserve = (resident / 64).min(self.budget.staging_bytes / 8);
        (resident - reserve).max(self.budget.region_bytes)
    }

    pub(super) fn staging_limit(&self, interactive: bool) -> usize {
        let staging = self.budget.staging_bytes;
        let reserve = if interactive {
            0
        } else {
            (staging / 4).min(self.budget.region_bytes)
        };
        self.budget.resident_bytes + (staging - reserve)
    }

    pub(super) fn staging_available(&self, interactive: bool) -> usize {
        (self.staging_limit(interactive) / size_of::<prime_scene::Quad>())
            .saturating_sub(self.ledger.owned)
    }

    pub(super) fn admit_compact_candidate(&mut self, index: usize) -> Result<bool, Error> {
        let job = &self.jobs[index];
        let handle = job.handle;
        let stride = size_of::<prime_scene::Quad>();
        let required = job.projected.saturating_mul(stride);
        self.capacity.remove_wait(handle);
        if required > self.budget.region_bytes {
            self.exclude_job(
                index,
                PressureKind::Region,
                required,
                self.budget.region_bytes,
            )?;
            return Ok(false);
        }
        let current = self
            .state
            .resource(handle)
            .filter(|_| self.state.is_visible(handle));
        let growth = job
            .projected
            .saturating_sub(current.map_or(0, RegionMesh::len));
        if growth == 0 {
            return Ok(true);
        }
        // 仅已有实际覆盖或已确认空区的交互更新可以使用常驻余量。
        let interactive = job.interactive
            && current
                .is_some_and(|old| old.len() != 0 || (!old.excluded && old.missing_sections == 0));
        let limit = self.resident_limit(interactive);
        let required = self
            .ledger
            .resident()
            .saturating_add(growth)
            .saturating_mul(stride);
        if required <= limit {
            return Ok(true);
        }
        // 保留旧覆盖和失效掩码，释放未发布quad。只在可容纳该次增长时重新签发。
        self.exclude_job(index, PressureKind::Resident, required, limit)?;
        self.capacity.park(handle, growth, interactive);
        Ok(false)
    }

    pub(super) fn wake_capacity_retries(&mut self) -> Result<(), Error> {
        let stride = size_of::<prime_scene::Quad>();
        let mut visits = self.work_budget.maintenance;
        let mut projected = self.ledger.resident();
        for interactive in [true, false] {
            let limit = self.resident_limit(interactive) / stride;
            while visits != 0
                && let Some(wait) = self
                    .capacity
                    .fitting(interactive, limit.saturating_sub(projected))
            {
                visits -= 1;
                self.capacity.remove_wait(wait.handle);
                if self
                    .state
                    .resource(wait.handle)
                    .is_some_and(|old| old.excluded)
                {
                    self.invalidate_handle(wait.handle, 0)?;
                    if wait.interactive {
                        self.edit_refreshes.insert(wait.handle)?;
                    }
                    projected += wait.quads;
                }
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) fn resident_retry_ready(&self) -> bool {
        let stride = size_of::<prime_scene::Quad>();
        [true, false].into_iter().any(|interactive| {
            let limit = self.resident_limit(interactive) / stride;
            self.capacity
                .fitting(interactive, limit.saturating_sub(self.ledger.resident()))
                .is_some()
        })
    }
}
