//! 可执行队列按同类入队次序服务。调用者在实际发出一次工作后记账。

use crate::{Error, ready_queue::ReadyQueue};
use std::hash::Hash;

pub(crate) struct PriorityQueue<K> {
    edits: ReadyQueue<K>,
    ordinary: ReadyQueue<K>,
    edit_turns: u8,
}

impl<K> Default for PriorityQueue<K> {
    fn default() -> Self {
        Self {
            edits: ReadyQueue::default(),
            ordinary: ReadyQueue::default(),
            edit_turns: 0,
        }
    }
}

impl<K: Copy + Eq + Hash> PriorityQueue<K> {
    pub fn insert(&mut self, key: K, interactive: bool) -> Result<bool, Error> {
        if interactive {
            let added = self.edits.insert(key)?;
            self.ordinary.remove(&key);
            Ok(added)
        } else if self.edits.contains(&key) {
            Ok(false)
        } else {
            self.ordinary.insert(key)
        }
    }

    pub fn promote(&mut self, key: K) -> Result<(), Error> {
        if self.ordinary.contains(&key) {
            self.insert(key, true)?;
        }
        Ok(())
    }

    pub fn contains(&self, key: &K, interactive: bool) -> bool {
        if interactive {
            self.edits.contains(key)
        } else {
            self.ordinary.contains(key)
        }
    }

    pub fn remove(&mut self, key: &K) {
        if !self.edits.remove(key) {
            self.ordinary.remove(key);
        }
    }

    pub fn select(&self, edits_ready: bool, ordinary_ready: bool) -> Option<bool> {
        match (
            edits_ready && !self.edits.is_empty(),
            ordinary_ready && !self.ordinary.is_empty(),
        ) {
            (true, true) => Some(self.edit_turns < 3),
            (true, false) => Some(true),
            (false, true) => Some(false),
            (false, false) => None,
        }
    }

    pub fn served(&mut self, interactive: bool) {
        self.edit_turns = if interactive {
            self.edit_turns.saturating_add(1).min(3)
        } else {
            0
        };
    }

    pub fn pop_first(&mut self) -> Option<K> {
        let interactive = self.select(true, true)?;
        self.served(interactive);
        if interactive {
            self.edits.pop_first()
        } else {
            self.ordinary.pop_first()
        }
    }

    pub fn lane(&self, interactive: bool) -> impl ExactSizeIterator<Item = &K> {
        if interactive {
            self.edits.iter()
        } else {
            self.ordinary.iter()
        }
    }

    pub const fn len(&self) -> usize {
        self.edits.len() + self.ordinary.len()
    }

    pub fn clear(&mut self) {
        self.edits.clear();
        self.ordinary.clear();
        self.edit_turns = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_work_keeps_a_service_opportunity_during_edit_backlog() -> Result<(), Error> {
        let mut queue = PriorityQueue::default();
        queue.insert(0, false)?;
        for edit in 1..=5 {
            queue.insert(edit, true)?;
        }
        assert_eq!(
            [(); 5].map(|()| queue.pop_first()),
            [Some(1), Some(2), Some(3), Some(0), Some(4)]
        );
        Ok(())
    }
}
