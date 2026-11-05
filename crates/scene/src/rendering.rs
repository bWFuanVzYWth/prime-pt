//! 宿主与原生会话共享的渲染选择；有效积分器由模式决定。

use crate::Error;

/// 固定的输出采样偏好；概率和历史上限不取决于观察到的样本亮度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderTuning {
    history_cap: u32,
    fresh_probability: f32,
    diagnostic: bool,
}

impl Default for RenderTuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl RenderTuning {
    /// 窗口默认与game相同：历史M上限20、fresh替换概率0.4，关闭诊断。
    pub const DEFAULT: Self = Self {
        history_cap: 20,
        fresh_probability: 0.4,
        diagnostic: false,
    };
    /// 离屏算法基线不替换输出，其他值同默认。
    pub const BASELINE: Self = Self {
        fresh_probability: 0.,
        ..Self::DEFAULT
    };

    /// 历史上限1..=100、有限概率0..=1；诊断只影响实时输出。
    /// # Errors
    /// 越界或非有限输入返回场景输入错误，不发布部分设置。
    pub const fn new(
        history_cap: u32,
        fresh_probability: f32,
        diagnostic: bool,
    ) -> Result<Self, Error> {
        if history_cap < 1 || history_cap > 100 {
            return Err(Error::Input("ReSTIR历史M上限须为1..=100"));
        }
        if !fresh_probability.is_finite() || fresh_probability < 0. || fresh_probability > 1. {
            return Err(Error::Input("ReSTIR输出替换概率须为0..=1内的有限值"));
        }
        Ok(Self {
            history_cap,
            fresh_probability,
            diagnostic,
        })
    }
    /// temporal MIS对历史M应用的固定上限，不修改源UCW。
    pub const fn history_cap(self) -> u32 {
        self.history_cap
    }
    /// 输出选择初采估计的独立概率；不回写reservoir历史或减少派发。
    pub const fn fresh_probability(self) -> f32 {
        self.fresh_probability
    }
    /// 原生分辨率、关闭重建且不跨帧累积RGB的实时诊断偏好。
    pub const fn diagnostic(self) -> bool {
        self.diagnostic
    }
}

/// Prime 内部的实时更新或冻结累积模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum RenderMode {
    /// 消费世界变化，使用选中的实时积分器。
    #[default]
    Realtime = 0,
    /// 固定已接受的场景、相机和光照，独立累积普通 PT。
    Offline = 1,
}

impl TryFrom<u32> for RenderMode {
    type Error = Error;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Realtime),
            1 => Ok(Self::Offline),
            _ => Err(Error::Input("未知渲染模式")),
        }
    }
}

/// 实时积分器偏好；离线不会改变此选择。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum RealtimeRenderer {
    /// 独立路径采样。
    #[default]
    PathTrace = 0,
    /// 默认 point profile 的时空路径复用。
    RestirPt = 1,
}

impl TryFrom<u32> for RealtimeRenderer {
    type Error = Error;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::PathTrace),
            1 => Ok(Self::RestirPt),
            _ => Err(Error::Input("未知实时渲染器")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tuning_accepts_probability_and_mass_endpoints() {
        assert!(RenderTuning::new(1, 0., true).is_ok());
        assert!(RenderTuning::new(100, 1., false).is_ok());
    }
    #[test]
    fn tuning_rejects_invalid_mass_or_nonfinite_probability() {
        for cap in [0, 101] {
            assert!(RenderTuning::new(cap, 0.4, false).is_err());
        }
        for probability in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert!(RenderTuning::new(20, probability, false).is_err());
        }
    }
}
