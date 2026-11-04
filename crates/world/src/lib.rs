//! 引擎区块调度的LOD0所有者；源编译、准备和GPU确认后的发布分别处理。
mod budget;
#[cfg(test)]
mod budget_tests;
mod changes;
mod chunks;
mod ledger;
mod ready_queue;
mod region;
mod source;
mod spatial;
mod store;
pub use budget::{
    Budget, PayloadStats, Pressure, PressureKind, WorkBudget, WorkProgress, WorkStats,
};
pub use changes::SourceChange;
pub use chunks::{CaptureRequest, PreparedGeneration, Scene};
pub use prime_scene::{Camera, Environment, Material, Mesh, Quad, WorldCamera};
use source::{Section, compile, decode};
/// 单个宿主借用源包的最大字节数。
pub const MAX_PACKET: usize = 256 * 1024 * 1024;
const MAX_VERTICES: usize = 16 * 1024 * 1024;
/// 源、调度或容量失败；失败保留已发布资源。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 源协议或预算不符合约定。
    #[error("{0}")]
    Input(&'static str),
    /// 场景相机或输入校验失败。
    #[error(transparent)]
    Scene(#[from] prime_scene::Error),
    /// 批量工作池初始化失败。
    #[error(transparent)]
    Workers(#[from] rayon::ThreadPoolBuildError),
    /// 区块配置失败。
    #[error(transparent)]
    Config(#[from] chunk_manager::ConfigError),
    /// 资源状态转换失败。
    #[error(transparent)]
    Chunks(#[from] chunk_manager::StateError),
    /// 规划输入失败。
    #[error(transparent)]
    Planning(#[from] chunk_manager::PlanError),
    /// 已准备Minecraft资源或压缩源编译失败，不触发运行时资源补建。
    #[error(transparent)]
    Minecraft(#[from] prime_minecraft::Error),
    /// 元数据或候选缓冲预留失败；尚未发布的输出由原所有者回收。
    #[error(transparent)]
    Allocation(#[from] std::collections::TryReserveError),
    /// 管理器按实际占用扩容失败，旧身份和资源保持有效。
    #[error(transparent)]
    Growth(#[from] chunk_manager::GrowthError),
}

#[cfg(test)]
#[path = "legacy_tests.rs"]
mod tests;
