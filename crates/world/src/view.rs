//! 有序可见目录只保存身份；候选合并与回滚不扫描State槽，也不拥有网格副本。
use super::*;
use std::iter::Peekable;
use std::slice;

#[derive(Clone, Copy)]
pub(super) struct View {
    pub key: [i32; 3],
    pub handle: Handle,
}

#[derive(Clone, Copy)]
pub(super) struct ViewChange {
    pub key: [i32; 3],
    pub handle: Handle,
    pub upsert: bool,
    pub nonempty: bool,
}

struct Entries<'a> {
    base: Peekable<slice::Iter<'a, View>>,
    changes: Peekable<slice::Iter<'a, ViewChange>>,
}
impl Iterator for Entries<'_> {
    type Item = (View, bool);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let take_change = match (self.base.peek(), self.changes.peek()) {
                (Some(a), Some(b)) => b.key <= a.key,
                (None, Some(_)) => true,
                (Some(_), None) => false,
                (None, None) => return None,
            };
            if take_change {
                let change = self.changes.next().expect("已检查变化");
                if self.base.peek().is_some_and(|old| old.key == change.key) {
                    self.base.next();
                }
                if change.upsert && change.nonempty {
                    return Some((
                        View {
                            key: change.key,
                            handle: change.handle,
                        },
                        true,
                    ));
                }
            } else {
                return self.base.next().map(|entry| (*entry, false));
            }
        }
    }
}

struct Regions<'a> {
    scene: &'a Scene,
    entries: Entries<'a>,
    remaining: usize,
}
impl<'a> Iterator for Regions<'a> {
    type Item = RenderRegion<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let (entry, prepared) = self.entries.next()?;
        self.remaining -= 1;
        Some(self.scene.region_view(entry, prepared))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for Regions<'_> {}

fn reject_token(state: &mut State<RegionMesh>, patch: Patch) -> Result<(), Error> {
    match state.reject_patch(patch.token) {
        Ok(()) | Err(cm::StateError::StalePatch) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn revoke_matching(
    state: &mut State<RegionMesh>,
    patches: &mut Vec<Patch>,
    matches: impl Fn(Change) -> bool,
) -> Result<(), Error> {
    let mut at = 0;
    while at < patches.len() {
        if matches(patches[at].change) {
            reject_token(state, patches[at])?;
            patches.swap_remove(at);
        } else {
            at += 1;
        }
    }
    Ok(())
}

impl Scene {
    pub(super) fn revoke_upsert(&mut self, handle: Handle) -> Result<(), Error> {
        let Some(value) = self.state.chunk(handle) else {
            return Ok(());
        };
        // GPU 已取得带版本的独占输入。取回世界候选前，先解除该区域本帧或上一帧保留的锁。
        for patches in [&mut self.pending, &mut self.parked] {
            revoke_matching(
                &mut self.state,
                patches,
                |c| matches!(c, Change::Activate(chunk) | Change::Replace(chunk) if chunk == value),
            )?;
        }
        self.view_changes.clear();
        self.view_len = self.view.len();
        self.view_membership_changed = false;
        Ok(())
    }

    fn publication_class(&self, change: Change) -> usize {
        usize::from(match change {
            Change::Activate(value) | Change::Replace(value) => self
                .candidate_epochs
                .get(&key(value))
                .is_some_and(|&(_, edit)| edit),
            _ => false,
        })
    }

    pub(super) fn reserve_publication_retry(&mut self, bounds: Bounds) -> Result<(), Error> {
        if self.publication_remaining == 0 {
            return Ok(());
        }
        let class = usize::from(!self.turn.is_multiple_of(4));
        let Some(change) = self.publication_retry[class].take() else {
            return Ok(());
        };
        let (value, replace) = match change {
            Change::Activate(value) => (value, false),
            Change::Replace(value) => (value, true),
            _ => unreachable!("只重试候选覆盖"),
        };
        if !bounds.intersects(value) || self.publication_class(change) != class {
            return Ok(());
        }
        let Some(handle) = self.state.find(value) else {
            return Ok(());
        };
        let patch = if replace {
            self.state.prepare_replace(handle)
        } else {
            self.state.prepare_activate(handle)
        };
        match patch {
            Ok(patch) => self.pending.push(patch),
            Err(cm::StateError::RetirementFull) => self.publication_retry[class] = Some(change),
            Err(cm::StateError::PendingPatch | cm::StateError::Transition) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(super) fn cancel_returned_removals(&mut self, bounds: Bounds) -> Result<(), Error> {
        // 返回需求域的区块在规划前解除卸载锁，无需等待发布额度。
        for patches in [&mut self.removes, &mut self.parked, &mut self.pending] {
            revoke_matching(
                &mut self.state,
                patches,
                |c| matches!(c, Change::Remove(value) if bounds.intersects(value)),
            )?;
        }
        Ok(())
    }

    pub(super) fn stage_view(&mut self) -> Result<(), Error> {
        self.view_changes.clear();
        self.view_changes.try_reserve(self.pending.len())?;
        for patch in &self.pending {
            for change in self.state.patch_resources(patch.token)? {
                let entry = match change {
                    cm::VisibleChange::Remove { handle, chunk } => ViewChange {
                        key: key(chunk),
                        handle,
                        upsert: false,
                        nonempty: false,
                    },
                    cm::VisibleChange::Upsert {
                        handle,
                        chunk,
                        resource,
                    } => ViewChange {
                        key: key(chunk),
                        handle,
                        upsert: true,
                        nonempty: resource.len() != 0,
                    },
                };
                self.view_changes.push(entry);
            }
        }
        self.view_changes.sort_unstable_by_key(|entry| entry.key);
        self.view_changes.dedup_by_key(|entry| entry.key);
        self.view_len = self.view.len();
        self.view_membership_changed = false;
        for change in &self.view_changes {
            let existed = self
                .view
                .binary_search_by_key(&change.key, |entry| entry.key)
                .is_ok();
            let exists = change.upsert && change.nonempty;
            if existed != exists {
                self.view_membership_changed = true;
                self.view_len = self.view_len - usize::from(existed) + usize::from(exists);
            }
        }
        if self.view_membership_changed {
            self.view_scratch.clear();
            self.view_scratch.try_reserve(self.view_len)?;
            self.view_scratch.extend(
                Entries {
                    base: self.view.iter().peekable(),
                    changes: self.view_changes.iter().peekable(),
                }
                .map(|(entry, _)| entry),
            );
        }
        Ok(())
    }

    /// 借助区块管理器准备已获常驻准入且依赖仍有效的实际覆盖变化。
    ///
    /// 缺源片段继承旧网格，容量和发布额度等待保持已发布覆盖，实际退出需求域的资源独立卸载。
    /// 多数轮先处理编辑候选，每四轮先处理普通候选。每类首个因工作量等待的候选
    /// 在该类优先轮有专用重试名额。单个超额 region 可独立发布。
    /// 宿主从准备视图建立 GPU 候选，期间保持世界输入和规划静止，随后调用
    /// [`Self::commit`] 或 [`Self::reject`] 结束本次事务。跨帧上传调用 [`Self::park_publication`]
    /// 交还借用视图；后续捕获可撤销已过期的区域令牌，其他区域仍可续传。
    pub fn prepare(&mut self) -> Result<bool, Error> {
        let resume = !self.parked.is_empty();
        if resume {
            // 新就绪的独立区域仍由状态机持有。上一批仍可提交时，优先完成同一组令牌。
            self.dismiss_view()?;
            std::mem::swap(&mut self.pending, &mut self.parked);
        }
        let bounds = self.state.config().bounds().expect("有界region域");
        if self.pending.len() > 1 {
            let edit_first = !self.turn.is_multiple_of(4);
            let epochs = &self.candidate_epochs;
            self.pending.sort_by_key(|patch| {
                let interactive = match patch.change {
                    Change::Activate(value) | Change::Replace(value) => {
                        epochs.get(&key(value)).is_some_and(|&(_, edit)| edit)
                    }
                    _ => false,
                };
                interactive != edit_first
            });
        }
        let limit = [
            self.work_budget.publish_geometry_quads,
            self.work_budget.publish_attribute_quads,
        ];
        let mut used = self.publication_work;
        let mut accepted = 0;
        for at in 0..self.pending.len() {
            let patch = self.pending[at];
            let valid = accepted < self.publication_remaining
                && self.state.validate_patch(patch.token).is_ok()
                && match patch.change {
                    Change::Activate(value) | Change::Replace(value) => {
                        bounds.intersects(value)
                            && self
                                .candidate_epochs
                                .get(&key(value))
                                .is_none_or(|&(epoch, _)| self.input_epoch == Some(epoch))
                            && self
                                .candidate_sources
                                .get(&key(value))
                                .is_none_or(|sources| {
                                    sources.iter().all(|(key, stamp)| {
                                        !matches!(
                                            self.compact.store.lookup(*key),
                                            crate::store::SourceState::Unknown
                                        ) && self.compact.store.stamp(*key) == *stamp
                                    })
                                })
                    }
                    Change::Remove(value) => !bounds.intersects(value),
                    _ => false,
                };
            let work = match patch.change {
                Change::Activate(value) | Change::Replace(value) if valid => {
                    self.publication_cost(value)
                }
                _ => [0; 2],
            };
            let next = std::array::from_fn(|i| used[i].saturating_add(work[i]));
            // 零成本变化与独立超额首项均可推进，超额首项之后只接受零成本项。
            let enabled = (0..2).all(|i| work[i] == 0 || limit[i] != 0);
            if valid
                && enabled
                && (work == [0; 2] || used == [0; 2] || (0..2).all(|i| next[i] <= limit[i]))
            {
                used = next;
                self.pending.swap(accepted, at);
                accepted += 1;
            } else {
                if valid && enabled {
                    let class = self.publication_class(patch.change);
                    self.publication_retry[class].get_or_insert(patch.change);
                }
                reject_token(&mut self.state, patch)?;
            }
        }
        self.pending.truncate(accepted);
        self.prepared_publication_work = used;
        let mut at = 0;
        while !resume && at < self.removes.len() && self.pending.len() < self.publication_remaining
        {
            let patch = self.removes[at];
            if self.state.validate_patch(patch.token).is_err() {
                self.removes.swap_remove(at);
                reject_token(&mut self.state, patch)?;
            } else if let Change::Remove(value) = patch.change
                && !bounds.intersects(value)
            {
                self.pending.push(self.removes.swap_remove(at));
            } else {
                at += 1;
            }
        }
        self.dirty |= !self.pending.is_empty();
        self.stage_view()?;
        Ok(self.dirty)
    }

    fn publication_cost(&self, value: Chunk) -> [usize; 2] {
        let handle = self.state.find(value).expect("已准备的节点仍保活");
        let candidate = self
            .state
            .prepared_resource(handle)
            .expect("已准备的候选仍保活");
        let old = self
            .state
            .resource(handle)
            .filter(|_| self.state.is_visible(handle));
        if old.is_none_or(|old| old.revision != candidate.revision) {
            return [candidate.len(); 2];
        }
        let old = old.expect("相同几何必有旧覆盖");
        if old.content_revision == candidate.content_revision {
            return [0; 2];
        }
        let mesh = candidate.over(old);
        if mesh.attribute_only() {
            [
                0,
                mesh.attribute_ranges().map(|range| range.quads.len()).sum(),
            ]
        } else {
            // 普通材质替换可能使原不透明BLAS需要覆盖判断，预留完整几何额度。
            [candidate.len(); 2]
        }
    }

    /// GPU接受后发布；此时释放被替换网格的常驻份额并使符合额度的等待项可重试。
    /// 宿主须在本次 [`Self::prepare`] 后保持输入静止，并确认旧 GPU 消费已完成。
    /// 资源转移和目录发布已预留容量，不分配、不复制Quad或统计全cut。
    /// 其后独立退休维护可能扩容来源退休通知Vec；这不改变已经完成的发布事务。
    pub fn commit(&mut self) -> Result<(), Error> {
        for patch in &self.pending {
            self.state.validate_patch(patch.token)?;
        }
        self.publication_work = self.prepared_publication_work;
        while let Some(patch) = self.pending.pop() {
            self.state
                .publish_with(patch.token, 0, RegionMesh::inherit)?;
            if let Change::Activate(value) | Change::Replace(value) = patch.change {
                self.remove_candidate_sources(key(value));
            }
            self.progress.publications += 1;
            self.publication_remaining -= 1;
        }
        for index in 0..self.view_changes.len() {
            let change = self.view_changes[index];
            if change.upsert {
                self.refresh_ledger(change.handle);
                self.refresh_tint_index(change.handle);
                if self
                    .state
                    .resource(change.handle)
                    .is_some_and(|resource| resource.excluded)
                {
                    self.capacity.activate_wait(change.handle);
                } else {
                    self.capacity.remove_wait(change.handle);
                }
            } else {
                self.ledger.remove(change.key, change.handle);
                self.capacity.remove_wait(change.handle);
                self.edit_refreshes.remove(&change.handle);
                self.masks.remove(&change.key);
                self.tinted.remove(&change.key);
                self.custom_tinted.remove(&change.key);
                self.tinted_spatial.remove(change.key);
                self.remove_candidate_sources(change.key);
                if let Some(partial) = self.recovered.remove(&change.key) {
                    self.ledger.owned -= partial.parts.len();
                }
            }
        }
        if self.view_membership_changed {
            std::mem::swap(&mut self.view, &mut self.view_scratch);
            self.view_scratch.clear();
        } else {
            for change in &self.view_changes {
                if change.upsert && change.nonempty {
                    let index = self
                        .view
                        .binary_search_by_key(&change.key, |entry| entry.key)
                        .expect("成员不变的目录");
                    self.view[index].handle = change.handle;
                }
            }
        }
        self.view_changes.clear();
        self.view_len = self.view.len();
        self.dirty = false;
        self.drain_retired();
        Ok(())
    }

    /// GPU拒绝只撤候选目录和patch；旧目录、片段与已完成候选仍归原所有者。
    pub fn reject(&mut self) -> Result<(), Error> {
        while let Some(patch) = self.parked.pop() {
            reject_token(&mut self.state, patch)?;
        }
        self.dismiss_view()
    }
    /// GPU 或包生产者仍在处理已选择的覆盖。保留其管理器令牌；下一轮继续捕获和建网，
    /// 并在重建借用视图时重新校验来源版本与目标域。失效令牌会走普通撤销路径。
    pub fn park_publication(&mut self) {
        assert!(self.parked.is_empty(), "一次只有一份已选择的覆盖");
        std::mem::swap(&mut self.pending, &mut self.parked);
        self.view_changes.clear();
        self.view_len = self.view.len();
        self.view_membership_changed = false;
    }
    pub(super) fn dismiss_view(&mut self) -> Result<(), Error> {
        while let Some(patch) = self.pending.pop() {
            reject_token(&mut self.state, patch)?;
        }
        self.view_changes.clear();
        self.view_len = self.view.len();
        self.view_membership_changed = false;
        Ok(())
    }

    fn region_view(&self, entry: View, prepared: bool) -> RenderRegion<'_> {
        let current = self.state.resource(entry.handle).expect("目录身份保活");
        let resource = if prepared {
            self.state
                .prepared_resource(entry.handle)
                .expect("已准备资源")
        } else {
            current
        };
        let mesh = if std::ptr::eq(current, resource) {
            resource.view()
        } else {
            resource.over(current)
        };
        RenderRegion {
            key: entry.key,
            geometry_revision: resource.revision,
            content_revision: resource.content_revision,
            mesh,
        }
    }

    /// 有序准备视图；只迭代已发布非空目录与本次实际变化，不扫描空槽。
    pub fn render_regions(&self) -> impl ExactSizeIterator<Item = RenderRegion<'_>> {
        Regions {
            scene: self,
            entries: Entries {
                base: self.view.iter().peekable(),
                changes: self.view_changes.iter().peekable(),
            },
            remaining: self.view_len,
        }
    }

    /// 本次准备的实际渲染增量；纯颜色更新无需构造全场景视图。
    pub fn render_changes(&self) -> impl ExactSizeIterator<Item = prime_scene::RenderChange<'_>> {
        self.view_changes.iter().map(|change| {
            if change.upsert && change.nonempty {
                prime_scene::RenderChange::Upsert(self.region_view(
                    View {
                        key: change.key,
                        handle: change.handle,
                    },
                    true,
                ))
            } else {
                prime_scene::RenderChange::Remove(change.key)
            }
        })
    }

    /// 兼容测量入口；借用准备视图，不复制几何。
    pub fn entries(&self) -> impl ExactSizeIterator<Item = ([i32; 3], u64, MeshView<'_>)> {
        self.render_regions()
            .map(|region| (region.key, region.geometry_revision, region.mesh))
    }
}
