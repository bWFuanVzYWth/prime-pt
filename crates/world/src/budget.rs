//! CPU quad payload的容量策略；不冒充源缓存、metadata、RSS或GPU显存总上限。
/// 可配置的最终常驻、过渡和单region逻辑payload上限，单位为字节。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// 当前目标域全部region的最终quad payload。
    pub resident_bytes: usize,
    /// 旧域和dirty替换部分额外保留的payload余量；不足时暂停替换，不撤旧覆盖。
    pub staging_bytes: usize,
    /// 单个region可提交的payload；不能大于staging_bytes。
    pub region_bytes: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            resident_bytes: 1024 * 1024 * 1024,
            staging_bytes: 256 * 1024 * 1024,
            region_bytes: 256 * 1024 * 1024,
        }
    }
}

/// 当前压力原因；已明确不可容纳的region与临时等待回收分开报告。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum PressureKind {
    /// 没有拒绝或等待。
    #[default]
    None = 0,
    /// 当前目标域的已知最终payload超出常驻预算。
    Resident = 1,
    /// 过渡联合payload等待额外容量或可安全发布的替换；未回复ticket及旧覆盖保持有效。
    Transition = 2,
    /// 单region输出超过独立region预算。
    Region = 3,
}

/// 不抛异常的CPU容量状态，供宿主图表、日志及配置界面消费。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pressure {
    /// 当前最近一次容量阻碍。
    pub kind: PressureKind,
    /// 当前已知需要的逻辑payload字节数。
    pub required_bytes: usize,
    /// 对应原因的配置上限。
    pub limit_bytes: usize,
    /// 明确因预算暂缓替换的region数量；有旧mesh时仍保留它，不充当空源。
    pub excluded_regions: usize,
    /// 因等待回收暂停返回的待捕获请求数。
    pub deferred_requests: usize,
}
