//! 三轴有序元数据索引；矩形前沿不遍历视距体积。
//!
//! 选择几何跨度最短的轴范围，再过滤另外两轴；代价取决于该轴候选数，
//! 共面或密度不均时最坏仍可能检查全部存活键，不声称严格按最终命中数计费。
//! 调用者的region/section坐标已由i32 block协议域约束，远离i32端点。
use chunk_manager::Bounds;
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

/// `outer \ inner` 的至多六个不相交盒；内部仅由已校验边界构造。
pub(crate) fn difference(outer: Bounds, inner: Bounds) -> Vec<Bounds> {
    let low: [i64; 3] =
        std::array::from_fn(|axis| outer.minimum()[axis].max(inner.minimum()[axis]));
    let high: [i64; 3] =
        std::array::from_fn(|axis| outer.maximum()[axis].min(inner.maximum()[axis]));
    if (0..3).any(|axis| low[axis] >= high[axis]) {
        return vec![outer];
    }
    let mut result = Vec::with_capacity(6);
    let mut remaining_low = outer.minimum();
    let mut remaining_high = outer.maximum();
    for axis in 0..3 {
        if remaining_low[axis] < low[axis] {
            let mut edge = remaining_high;
            edge[axis] = low[axis];
            result.push(Bounds::new(remaining_low, edge).expect("非空前沿盒"));
            remaining_low[axis] = low[axis];
        }
        if high[axis] < remaining_high[axis] {
            let mut edge = remaining_low;
            edge[axis] = high[axis];
            result.push(Bounds::new(edge, remaining_high).expect("非空前沿盒"));
            remaining_high[axis] = high[axis];
        }
    }
    result
}
