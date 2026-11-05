//! 所有时间消费者的显式失效入口；绑定更新、参数发布和帧取消自身不重置已接受历史。
//!
//! 与game相同，运动/FOV、anchor、太阳、介质、seed、路径预算和局部编辑不能宣布整个
//! 重投影域失效。ReSTIR用稳定几何世代局部拒绝已删除/重建的表面；RR消费当前guide。
//! 缓存后缀辐射没有完整更新动态光照，保留历史不保证动态场景无偏或没有鬼影，M cap也不是TTL。
//! 普通PT的静态RGB均值另有输入失效和逐帧输出事件，不能借此清除RR或路径历史。
//! 曝光适应不属于这些复用域，所有事件均保留它。存储真实替换单独记录，不伪装成内容变化。

use super::{Error, Gpu};
use simple_engine::pt;

/// 确认整个语义域不可继续使用的封闭事件；常规帧更新不能构造其他理由。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalReset {
    /// 切换世界，旧帧不再属于同一世界。
    WorldReplaced,
    /// 模型/材质身份命名空间整体替换。
    SceneDomainReplaced,
    /// 实时/离线或积分器切换，整个输出域改变。
    RenderDomainChanged,
    /// 调用者明确请求丢弃全部时间历史。
    ExplicitRequested,
}

impl GlobalReset {
    const fn name(self) -> &'static str {
        match self {
            Self::WorldReplaced => "world_replaced",
            Self::SceneDomainReplaced => "scene_domain_replaced",
            Self::RenderDomainChanged => "render_domain_changed",
            Self::ExplicitRequested => "explicit_request",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccumulationEvent {
    SceneChanged,
    MediumChanged,
    FrameInputsChanged,
    PerFrameOutput,
    OutputModeChanged,
    ExplicitRequested,
    RecordingCancelled,
}

/// 实际更换视图或SDK存储，不代表重投影的其他消费者也失效。
#[derive(Clone, Copy, Debug)]
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

#[derive(Clone, Copy, Debug)]
pub(super) enum Event {
    Global(GlobalReset),
    Accumulation(AccumulationEvent),
    ViewStorage(StorageEvent),
    RrStorage(StorageEvent),
    RestirStorage(StorageEvent),
    RrEvaluationFailed,
}

/// 只有本模块能创建；路径工作集不接受无理由的reset调用。
pub(super) struct ResetPermit(());

/// 只用固定键推进样本；输入比较和重置由Gpu的显式事件入口负责。
pub(super) struct Accumulation {
    progress: pt::Progress<()>,
    key: Option<[u32; 25]>,
}

impl Accumulation {
    pub const fn new() -> Self {
        Self {
            progress: pt::Progress::new(),
            key: None,
        }
    }
    pub const fn samples(&self) -> u32 {
        self.progress.samples()
    }
    pub fn begin(&mut self, samples: u32) -> Result<pt::Batch<'_, ()>, pt::Error> {
        self.progress.begin((), samples)
    }
}

#[derive(Default)]
pub(super) struct Audit {
    pub accumulation_resets: u64,
    pub rr_resets: u64,
    pub restir_resets: u64,
    pub view_storage: u64,
    pub restir_storage: u64,
    pub last_accumulation: Option<AccumulationEvent>,
}

fn record(consumer: &str, event: &str, action: &str, valid: bool) {
    eprintln!("[prime_pt] {consumer}.history event={event} action={action} valid={valid}");
}

impl Gpu {
    /// 唯一失效执行点。调用者已完成旧帧，取消时须先reject待接受的路径帧。
    pub(super) fn transition_history(&mut self, event: Event) -> Result<(), Error> {
        let global = matches!(event, Event::Global(_));
        if global && self.restir.as_ref().is_some_and(|r| r.has_pending()) {
            return Err(simple_engine::restir_pt::Error::PendingFrame.into());
        }
        let accumulation =
            global || matches!(event, Event::Accumulation(_) | Event::ViewStorage(_));
        if accumulation {
            if let Some(view) = &mut self.view {
                match event {
                    Event::Global(reason) => {
                        record("pt", reason.name(), "reset", view.progress.samples() != 0)
                    }
                    Event::ViewStorage(reason) => record(
                        "pt",
                        reason.name(),
                        "storage_cold",
                        view.progress.samples() != 0,
                    ),
                    _ => {}
                }
                view.progress.progress.reset();
                view.progress.key = None;
            }
            self.history_audit.accumulation_resets =
                self.history_audit.accumulation_resets.saturating_add(1);
        }
        if global
            || matches!(
                event,
                Event::ViewStorage(_) | Event::RrStorage(_) | Event::RrEvaluationFailed
            )
        {
            let (name, action) = match event {
                Event::Global(reason) => (reason.name(), "reset"),
                Event::ViewStorage(reason) | Event::RrStorage(reason) => {
                    (reason.name(), "storage_cold")
                }
                Event::RrEvaluationFailed => ("evaluation_failed", "recovery_reset"),
                _ => unreachable!("仅上述事件影响RR"),
            };
            record("rr", name, action, self.rr_previous.is_some());
            self.rr_previous = None;
            self.history_audit.rr_resets = self.history_audit.rr_resets.saturating_add(1);
        }
        if let Event::Global(reason) = event {
            if let Some(restir) = &mut self.restir {
                record("restir", reason.name(), "reset", restir.has_history());
                restir.invalidate(ResetPermit(()))?;
            }
            self.history_audit.restir_resets = self.history_audit.restir_resets.saturating_add(1);
        }
        match event {
            Event::ViewStorage(_) => {
                self.history_audit.view_storage = self.history_audit.view_storage.saturating_add(1)
            }
            Event::RestirStorage(reason) => {
                record(
                    "restir",
                    reason.name(),
                    "storage_cold",
                    self.restir
                        .as_ref()
                        .is_some_and(|r| r.previous_camera.is_some()),
                );
                self.history_audit.restir_storage =
                    self.history_audit.restir_storage.saturating_add(1);
            }
            Event::Accumulation(reason) => self.history_audit.last_accumulation = Some(reason),
            _ => {}
        }
        Ok(())
    }

    /// 决策与执行都留在本模块；begin始终使用固定键，不存在暗中清零的比较器。
    pub(super) fn prepare_accumulation_history(
        &mut self,
        key: [u32; 25],
        per_frame: bool,
    ) -> Result<(), Error> {
        let view = self.view.as_ref().expect("已建立视图");
        let event = if per_frame {
            Some(AccumulationEvent::PerFrameOutput)
        } else if view.progress.key.is_some_and(|previous| previous != key) {
            Some(AccumulationEvent::FrameInputsChanged)
        } else {
            None
        };
        if let Some(event) = event {
            self.transition_history(Event::Accumulation(event))?;
        }
        self.view.as_mut().unwrap().progress.key = Some(key);
        Ok(())
    }
}
