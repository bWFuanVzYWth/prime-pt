//! 宿主只声明实际源变化；依赖传播、去重和候选保活由 world 统一维护。

/// 批量变化的类型边界；坐标以绝对 16m section 计。
///
/// 一批输入先完整校验，再改变来源有效性。通知声明实际写入的域，下一次捕获确定
/// 可用性和内容差分。重叠的整列通知可以合并失效范围，局部写入身份仍独立保留。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceChange {
    /// 实际 block 源变化；`minimum < maximum <= 16`，各轴独立。
    /// 视域内的局部写入由调度器按当前相机分类。
    BlockRange {
        /// 所在 section。
        section: [i32; 3],
        /// 各轴包含的最小 block 坐标。
        minimum: [u8; 3],
        /// 各轴不包含的最大 block 坐标。
        maximum: [u8; 3],
    },
    /// 已知整个 section 发生源替换，供拥有完整段级信息的适配者使用。
    SectionSource([i32; 3]),
    /// 整列替换或卸载的半开高度域；几何与 biome 依赖分别声明。
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
