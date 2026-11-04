//! 方块源变化到编译 slab 的依赖范围；这里只计算版本规则，不调度或保存候选。

use crate::Error;

/// 一个位于 16m section 内的编译依赖；低四位分别对应 Y=0、4、8、12 的 slab。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlabDependency {
    /// 需要重新编译的输出 section 坐标。
    pub section: [i32; 3],
    /// 非零四位集合；同 section 的多项事件由调度所有者取并集。
    pub slabs: u8,
}

/// 一次源范围变化的最多 27 项依赖；自持固定数组，不为每个事件分配堆内存。
#[derive(Clone, Debug)]
pub struct SlabDependencies {
    entries: [SlabDependency; 27],
    len: u8,
    next: u8,
}

impl Iterator for SlabDependencies {
    type Item = SlabDependency;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.len {
            return None;
        }
        let value = self.entries[usize::from(self.next)];
        self.next += 1;
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = usize::from(self.len - self.next);
        (count, Some(count))
    }
}

impl ExactSizeIterator for SlabDependencies {}
impl std::iter::FusedIterator for SlabDependencies {}

/// 将真实方块源范围映射到受影响的输出 section/slab。
///
/// `min..max_exclusive` 是同一 section 内非空、各轴位于 `0..=16` 的范围。
/// 模型剔面读取六个直接邻块，流体高度和流向读取半径一方块邻域，故统一取
/// Chebyshev 半径一的保守并集；不扩张成整片相邻 section。自定义 tint 的任意
/// 世界依赖不由此函数缩域，由其独立绑定失效策略维护。
///
/// 输入范围非法或依赖 section 坐标溢出时返回错误，尚未向调用者发布任何项。
pub fn dependent_slabs(
    section: [i32; 3],
    min: [u8; 3],
    max_exclusive: [u8; 3],
) -> Result<SlabDependencies, Error> {
    if (0..3).any(|axis| min[axis] >= max_exclusive[axis] || max_exclusive[axis] > 16) {
        return Err(Error::Input("方块变化范围须为section内非空范围"));
    }
    let low = min.map(|value| i32::from(value) - 1);
    let high = max_exclusive.map(|value| i32::from(value) + 1);
    let mut result = SlabDependencies {
        entries: [SlabDependency::default(); 27],
        len: 0,
        next: 0,
    };
    for x in low[0].div_euclid(16)..=(high[0] - 1).div_euclid(16) {
        for y in low[1].div_euclid(16)..=(high[1] - 1).div_euclid(16) {
            let first = (low[1] - y * 16).max(0) / 4;
            let last = ((high[1] - 1 - y * 16).min(15)) / 4;
            let slabs = (((1u16 << (last - first + 1)) - 1) << first) as u8;
            for z in low[2].div_euclid(16)..=(high[2] - 1).div_euclid(16) {
                let mut key = [0; 3];
                for (axis, offset) in [x, y, z].into_iter().enumerate() {
                    key[axis] = section[axis]
                        .checked_add(offset)
                        .ok_or(Error::Input("方块变化依赖section坐标溢出"))?;
                }
                result.entries[usize::from(result.len)] = SlabDependency {
                    section: key,
                    slabs,
                };
                result.len += 1;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interior_and_vertical_slab_edge_use_only_true_block_neighborhood() {
        assert_eq!(
            dependent_slabs([2, -3, 4], [7, 7, 7], [8, 8, 8])
                .unwrap()
                .collect::<Vec<_>>(),
            vec![SlabDependency {
                section: [2, -3, 4],
                slabs: 0b0110,
            }]
        );
        assert_eq!(
            dependent_slabs([2, -3, 4], [7, 5, 7], [8, 6, 8])
                .unwrap()
                .collect::<Vec<_>>(),
            vec![SlabDependency {
                section: [2, -3, 4],
                slabs: 0b0010,
            }]
        );
    }

    #[test]
    fn section_corner_and_whole_section_preserve_neighbor_slab_masks() {
        let corner: Vec<_> = dependent_slabs([-1, -1, -1], [0; 3], [1; 3])
            .unwrap()
            .collect();
        assert_eq!(corner.len(), 8);
        for item in corner {
            assert!(
                item.section
                    .into_iter()
                    .all(|value| value == -2 || value == -1)
            );
            assert_eq!(item.slabs, if item.section[1] == -2 { 8 } else { 1 });
        }
        let whole: Vec<_> = dependent_slabs([0; 3], [0; 3], [16; 3]).unwrap().collect();
        assert_eq!(whole.len(), 27);
        for item in whole {
            assert_eq!(item.slabs, [8, 15, 1][(item.section[1] + 1) as usize]);
        }
    }

    #[test]
    fn invalid_ranges_or_coordinate_overflow_publish_no_partial_iterator() {
        assert!(dependent_slabs([0; 3], [1; 3], [1; 3]).is_err());
        assert!(dependent_slabs([0; 3], [0; 3], [17; 3]).is_err());
        assert!(dependent_slabs([i32::MAX; 3], [15; 3], [16; 3]).is_err());
    }
}
