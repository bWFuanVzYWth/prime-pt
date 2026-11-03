//! RR全画面历史的封闭事件；局部内容变化通过当前场景、guide与重投影继续更新。
//!
//! 保留历史是应用对观感的取舍，不证明SDK逐像素拒绝或任意动态场景无偏。
//! PT静态线性累积独立失效；实际视图/SDK存储替换与失败恢复也单独记录。

/// 调用者确认整个历史语义域不可继续解释的事件。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GlobalReset {
    /// 切换世界；旧帧不再属于同一世界。
    WorldReplaced,
    /// 模型/材质等语义域整体替换，旧对应关系失效。
    SceneDomainReplaced,
}

impl GlobalReset {
    const fn name(self) -> &'static str {
        match self {
            Self::WorldReplaced => "world_replaced",
            Self::SceneDomainReplaced => "scene_domain_replaced",
        }
    }
}

/// 视图或SDK的实际存储替换；不视为普通内容变化触发的全局事件。
#[derive(Clone, Copy)]
pub(super) enum StorageEvent {
    Initial,
    ExtentChanged,
    QualityChanged,
    ReconstructionChanged,
    RuntimeRecreated,
}

impl StorageEvent {
    const fn name(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::ExtentChanged => "extent_changed",
            Self::QualityChanged => "quality_changed",
            Self::ReconstructionChanged => "reconstruction_changed",
            Self::RuntimeRecreated => "runtime_recreated",
        }
    }
}

pub(super) fn record_global(event: GlobalReset, valid: bool) {
    eprintln!(
        "[prime_pt] rr.history event={} action=reset valid={valid}",
        event.name()
    );
}

pub(super) fn record_storage(event: StorageEvent, valid: bool) {
    eprintln!(
        "[prime_pt] rr.history event={} action=storage_cold valid={valid}",
        event.name()
    );
}

pub(super) fn record_recovery(valid: bool) {
    eprintln!("[prime_pt] rr.history event=evaluation_failed action=recovery_reset valid={valid}");
}
