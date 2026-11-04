//! 可执行工作按真实入队年龄推进；取消物理删除，重复通知不抢占已有队龄。
use crate::{Error, store::WorkId};
use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
};

pub(crate) struct ReadyQueue<K = WorkId> {
    order: BTreeMap<u64, K>,
    by_work: HashMap<K, u64>,
    serial: u64,
}

impl<K> Default for ReadyQueue<K> {
    fn default() -> Self {
        Self {
            order: BTreeMap::new(),
            by_work: HashMap::new(),
            serial: 0,
        }
    }
}

impl<K: Copy + Eq + Hash> ReadyQueue<K> {
    /// 仅预备后续新增/轮转的序号空间和哈希容量；不改变队龄或签发序号。
    /// 调用者须计入其间所有入队操作；树节点仍遵循标准库OOM终止边界。
    pub fn reserve_insertions(&mut self, additional: usize) -> Result<(), Error> {
        let count = u64::try_from(additional).map_err(|_| Error::Input("工作队列预留数量越界"))?;
        self.serial
            .checked_add(count)
            .ok_or(Error::Input("工作队列入队身份耗尽"))?;
        self.by_work.try_reserve(additional)?;
        Ok(())
    }

    /// 先检查身份容量并预留哈希目录；树节点分配遵循标准库OOM终止边界。
    pub fn insert(&mut self, work: K) -> Result<bool, Error> {
        if self.by_work.contains_key(&work) {
            return Ok(false);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(Error::Input("工作队列入队身份耗尽"))?;
        self.by_work.try_reserve(1)?;
        self.order.insert(serial, work);
        self.by_work.insert(work, serial);
        self.serial = serial;
        Ok(true)
    }

    pub fn remove(&mut self, work: &K) -> bool {
        let Some(serial) = self.by_work.remove(work) else {
            return false;
        };
        let removed = self.order.remove(&serial);
        debug_assert!(
            removed.is_some_and(|key| key == *work),
            "ready双索引由唯一所有者共同维护"
        );
        true
    }

    /// 已有工作轮转到队尾；缺失返回false，序号耗尽保留原队列。
    pub fn renew(&mut self, work: &K) -> Result<bool, Error> {
        let Some(&old) = self.by_work.get(work) else {
            return Ok(false);
        };
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(Error::Input("工作队列入队身份耗尽"))?;
        let removed = self.order.remove(&old);
        debug_assert!(removed.is_some_and(|key| key == *work));
        self.order.insert(serial, *work);
        *self.by_work.get_mut(work).expect("轮转保留原有键") = serial;
        self.serial = serial;
        Ok(true)
    }

    pub fn pop_first(&mut self) -> Option<K> {
        let (serial, work) = self.order.pop_first()?;
        let removed = self.by_work.remove(&work);
        debug_assert_eq!(removed, Some(serial));
        Some(work)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &K> {
        self.order.values()
    }

    pub const fn len(&self) -> usize {
        self.order.len()
    }
    pub const fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.by_work.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn work(x: i32) -> WorkId {
        WorkId {
            region: [x, 0, 0],
            fragment: 0,
            epoch: 1,
        }
    }

    #[test]
    fn duplicate_notification_keeps_original_age_instead_of_coordinate_priority()
    -> Result<(), Error> {
        let mut queue = ReadyQueue::default();
        queue.insert(work(100))?;
        queue.insert(work(-100))?;
        assert!(!queue.insert(work(100))?);
        assert_eq!(
            queue.iter().copied().collect::<Vec<_>>(),
            [work(100), work(-100)]
        );
        assert_eq!(queue.pop_first(), Some(work(100)));
        assert_eq!(queue.pop_first(), Some(work(-100)));
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn cancel_and_reinsert_yields_to_work_already_waiting() -> Result<(), Error> {
        let mut queue = ReadyQueue::default();
        queue.insert(work(0))?;
        queue.insert(work(10))?;
        assert!(queue.remove(&work(0)));
        assert!(!queue.remove(&work(0)));
        queue.insert(work(0))?;
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop_first(), Some(work(10)));
        assert_eq!(queue.pop_first(), Some(work(0)));
        Ok(())
    }

    #[test]
    fn repeatedly_refreshed_near_work_cannot_starve_existing_far_work() -> Result<(), Error> {
        let mut queue = ReadyQueue::default();
        queue.insert(work(0))?;
        queue.insert(work(1000))?;
        for _ in 0..100 {
            queue.remove(&work(0));
            queue.insert(work(0))?;
        }
        assert_eq!(queue.pop_first(), Some(work(1000)));
        assert_eq!(queue.pop_first(), Some(work(0)));
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn exhausted_serial_rejects_before_mutating_existing_work() -> Result<(), Error> {
        let mut queue = ReadyQueue::default();
        queue.insert(work(1))?;
        queue.serial = u64::MAX;
        assert!(queue.insert(work(2)).is_err());
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop_first(), Some(work(1)));
        queue.clear();
        assert!(queue.insert(work(2)).is_err());
        Ok(())
    }

    #[test]
    fn issued_work_rotation_preserves_fifo_for_other_consumers() -> Result<(), Error> {
        let mut queue = ReadyQueue::<u32>::default();
        queue.reserve_insertions(3)?;
        for key in [10, 20, 30] {
            queue.insert(key)?;
        }
        assert!(queue.renew(&10)?);
        assert!(!queue.renew(&99)?);
        assert_eq!(queue.iter().copied().collect::<Vec<_>>(), [20, 30, 10]);
        queue.serial = u64::MAX;
        assert!(queue.renew(&20).is_err());
        assert!(queue.reserve_insertions(1).is_err());
        assert_eq!(queue.pop_first(), Some(20));
        Ok(())
    }
}
