//! 宿主与原生会话共享的渲染选择；实时与离线均使用普通 PT。

use crate::Error;

/// 原生分辨率、关闭重建且不跨帧累积 RGB 的实时诊断偏好。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderTuning {
    diagnostic: bool,
}
impl RenderTuning {
    /// 默认保留实时重建。
    pub const DEFAULT: Self = Self::new(false);
    /// 设置原始单帧诊断；离线模式忽略该偏好。
    pub const fn new(diagnostic: bool) -> Self {
        Self { diagnostic }
    }
    /// 是否使用实时原始单帧输出。
    pub const fn diagnostic(self) -> bool {
        self.diagnostic
    }
}

/// Prime 内部的实时更新或冻结累积模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum RenderMode {
    /// 消费世界变化，使用普通 PT。
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
