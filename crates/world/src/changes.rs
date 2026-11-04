//! 宿主只声明实际源变化；依赖传播、去重和候选保活由 world 统一维护。

/// 批量变化的类型边界；坐标以绝对 16m section 计。
///
/// 一批输入先完整校验，再改变来源与依赖。`BlockRange` 的端点是 section 内半开
/// block 范围；来源不是颜色缓存，普通方块变化不会隐式清除原版 biome 页。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceChange {
    /// 实际 block 源变化；`minimum < maximum <= 16`，各轴独立。
    BlockRange {
        /// 所在 section。
        section: [i32; 3],
        /// 各轴包含的最小 block 坐标。
        minimum: [u8; 3],
        /// 各轴不包含的最大 block 坐标。
        maximum: [u8; 3],
    },
    /// section 源整体变化或加载状态变化。
    SectionSource([i32; 3]),
    /// 一列来源的半开高度域；几何与 biome 依赖分别声明。
    ColumnSource {
        /// section 列 X。
        x: i32,
        /// section 列 Z。
        z: i32,
        /// 包含的最小 section Y。
        minimum_y: i32,
        /// 不包含的最大 section Y。
        maximum_y: i32,
        /// 压缩方块源是否变化。
        geometry: bool,
        /// 原始 biome 身份是否变化。
        biome: bool,
    },
    /// 原始 biome 列变化；不丢弃方块源或模型几何。
    BiomeColumn {
        /// section 列 X。
        x: i32,
        /// section 列 Z。
        z: i32,
    },
    /// 颜色/custom 依赖整体变化；保留压缩方块源与几何。
    WorldTint,
    /// 来源通知丢失；保守刷新 source、biome 和 tint，保留旧发布覆盖。
    SourceOverflow,
}
