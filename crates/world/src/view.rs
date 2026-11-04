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

impl Scene {
    pub(super) fn cancel_returned_removals(&mut self, bounds: Bounds) -> Result<(), Error> {
        // 返回需求域的区块在规划前解除卸载锁，无需等待发布额度。
        let mut at = 0;
        while at < self.removes.len() {
            if matches!(self.removes[at].change, Change::Remove(value) if bounds.intersects(value))
            {
                let patch = self.removes.swap_remove(at);
                self.reject_token(patch)?;
            } else {
                at += 1;
            }
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

    /// 只准备实际patch；来源或预算等待不撤旧几何，退出Bounds的资源独立卸载。
    pub fn prepare(&mut self) -> Result<bool, Error> {
        let bounds = self.state.config().bounds().expect("有界region域");
        let mut at = 0;
        while at < self.pending.len() {
            let patch = self.pending[at];
            let valid = at < self.publication_remaining
                && self.state.validate_patch(patch.token).is_ok()
                && match patch.change {
                    Change::Activate(value) | Change::Replace(value) => {
                        bounds.intersects(value)
                            && self
                                .candidate_epochs
                                .get(&key(value))
                                .is_none_or(|epoch| self.input_epoch == Some(*epoch))
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
                    _ => false,
                };
            if valid {
                at += 1;
            } else {
                let patch = self.pending.swap_remove(at);
                self.reject_token(patch)?;
            }
        }
        let mut at = 0;
        while at < self.removes.len() && self.pending.len() < self.publication_remaining {
            let patch = self.removes[at];
            if self.state.validate_patch(patch.token).is_err() {
                self.removes.swap_remove(at);
                self.reject_token(patch)?;
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

    /// GPU接受后发布；资源转移和目录发布已预留容量，不分配、不复制Quad或统计全cut。
    /// 其后独立退休维护可能扩容来源退休通知Vec；这不改变已经完成的发布事务。
    pub fn commit(&mut self) -> Result<(), Error> {
        for patch in &self.pending {
            self.state.validate_patch(patch.token)?;
        }
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
            } else {
                self.ledger.remove(change.key, change.handle);
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
        while let Some(patch) = self.pending.pop() {
            self.reject_token(patch)?;
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
