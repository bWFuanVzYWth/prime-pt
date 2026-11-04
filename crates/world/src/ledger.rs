//! 发布、候选与退役的精确逻辑账；这里只保存键和计数，不拥有第二份网格。
use crate::spatial::{Key, SpatialIndex, contains, difference};
use chunk_manager::{Bounds, Handle};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub(crate) struct Contribution {
    pub handle: Handle,
    pub projected: usize,
    pub excluded: bool,
}

#[derive(Default)]
pub(crate) struct Ledger {
    pub owned: usize,
    projected: usize,
    excluded: usize,
    bounds: Option<Bounds>,
    records: BTreeMap<Key, Contribution>,
    excluded_keys: BTreeSet<Key>,
    spatial: SpatialIndex,
}

impl Ledger {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub const fn projected(&self) -> usize {
        self.projected
    }

    pub const fn excluded(&self) -> usize {
        self.excluded
    }

    pub fn contribution(&self, key: Key) -> Option<Contribution> {
        self.records.get(&key).copied()
    }

    pub fn keys(&self, bounds: Bounds) -> Vec<Key> {
        self.spatial.keys(bounds)
    }

    pub fn update(&mut self, key: Key, value: Contribution) {
        if value.excluded {
            self.excluded_keys.insert(key);
        } else {
            self.excluded_keys.remove(&key);
        }
        if let Some(previous) = self.records.insert(key, value) {
            if self.bounds.is_some_and(|bounds| contains(bounds, key)) {
                self.projected -= previous.projected;
                self.excluded -= usize::from(previous.excluded);
            }
        } else {
            self.spatial.insert(key);
        }
        if self.bounds.is_some_and(|bounds| contains(bounds, key)) {
            self.projected += value.projected;
            self.excluded += usize::from(value.excluded);
        }
    }

    pub fn remove(&mut self, key: Key, handle: Handle) {
        if !self
            .records
            .get(&key)
            .is_some_and(|entry| entry.handle == handle)
        {
            return;
        }
        let previous = self.records.remove(&key).expect("身份匹配的账项");
        self.excluded_keys.remove(&key);
        self.spatial.remove(key);
        if self.bounds.is_some_and(|bounds| contains(bounds, key)) {
            self.projected -= previous.projected;
            self.excluded -= usize::from(previous.excluded);
        }
    }

    pub fn excluded_handles(&self) -> impl Iterator<Item = Handle> + '_ {
        self.excluded_keys.iter().filter_map(|key| {
            self.bounds
                .filter(|bounds| contains(*bounds, *key))
                .map(|_| self.records[key].handle)
        })
    }

    pub fn set_bounds(&mut self, bounds: Bounds) {
        if self.bounds == Some(bounds) {
            return;
        }
        if let Some(previous) = self.bounds {
            for edge in difference(previous, bounds) {
                for key in self.spatial.keys(edge) {
                    let value = self.records[&key];
                    self.projected -= value.projected;
                    self.excluded -= usize::from(value.excluded);
                }
            }
            for edge in difference(bounds, previous) {
                for key in self.spatial.keys(edge) {
                    let value = self.records[&key];
                    self.projected += value.projected;
                    self.excluded += usize::from(value.excluded);
                }
            }
        } else {
            for key in self.spatial.keys(bounds) {
                let value = self.records[&key];
                self.projected += value.projected;
                self.excluded += usize::from(value.excluded);
            }
        }
        self.bounds = Some(bounds);
    }
}
