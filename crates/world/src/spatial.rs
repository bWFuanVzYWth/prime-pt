//! 三轴有序元数据索引；矩形前沿不遍历视距体积。
//!
//! 选择几何跨度最短的轴范围，再过滤另外两轴；代价取决于该轴候选数，
//! 共面或密度不均时最坏仍可能检查全部存活键，不声称严格按最终命中数计费。
//! 调用者的region/section坐标已由i32 block协议域约束，远离i32端点。
use simple_engine::chunk_manager::Bounds;
use std::collections::BTreeSet;

pub(crate) type Key = [i32; 3];

#[derive(Default)]
pub(crate) struct SpatialIndex {
    axes: [BTreeSet<Key>; 3],
}

impl SpatialIndex {
    pub fn insert(&mut self, key: Key) {
        for (axis, entries) in self.axes.iter_mut().enumerate() {
            entries.insert(rotated(key, axis));
        }
    }

    pub fn remove(&mut self, key: Key) {
        for (axis, entries) in self.axes.iter_mut().enumerate() {
            entries.remove(&rotated(key, axis));
        }
    }

    pub fn clear(&mut self) {
        for axis in &mut self.axes {
            axis.clear();
        }
    }

    pub fn keys(&self, bounds: Bounds) -> Vec<Key> {
        let minimum = bounds.minimum();
        let maximum = bounds.maximum();
        let axis = (0..3)
            .min_by_key(|axis| maximum[*axis] - minimum[*axis])
            .expect("三个轴");
        let start = [minimum[axis], i64::MIN, i64::MIN];
        let end = [maximum[axis], i64::MIN, i64::MIN];
        let low = start.map(|v| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
        let high = end.map(|v| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
        self.axes[axis]
            .range(low..high)
            .map(|value| unrotated(*value, axis))
            .filter(|key| contains(bounds, *key))
            .collect()
    }
}

pub(crate) fn contains(bounds: Bounds, key: Key) -> bool {
    (0..3).all(|axis| {
        i64::from(key[axis]) >= bounds.minimum()[axis]
            && i64::from(key[axis]) < bounds.maximum()[axis]
    })
}

fn rotated(key: Key, axis: usize) -> Key {
    [key[axis], key[(axis + 1) % 3], key[(axis + 2) % 3]]
}

fn unrotated(value: Key, axis: usize) -> Key {
    let mut key = [0; 3];
    for offset in 0..3 {
        key[(axis + offset) % 3] = value[offset];
    }
    key
}

/// 引擎的无分配半开盒差集。
pub(crate) fn difference(outer: Bounds, inner: Bounds) -> impl ExactSizeIterator<Item = Bounds> {
    outer.difference(inner)
}
