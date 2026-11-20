//! 帧输入的有限执行计划；程序、采样身份与后处理选择由同一所有者建立。
use simple_engine::pt::gpu::RealtimeSchedule;

/// 三类精确合成图；不会改变天空或介质的数值模型。
#[derive(Clone, Copy)]
pub(super) enum Composite {
    None,
    Clear,
    Shadowed,
}

pub(super) struct FramePlan {
    pub realtime: Option<RealtimeSchedule>,
    pub primary: usize,
    pub post: usize,
}

impl FramePlan {
    pub const fn new(realtime: bool, split: bool, frame_film: bool, composite: Composite) -> Self {
        let schedule = if !realtime {
            None
        } else if split {
            Some(RealtimeSchedule::SplitTwo)
        } else {
            Some(RealtimeSchedule::MergedTwo)
        };
        let two = realtime as usize;
        let post = match composite {
            Composite::None => two,
            Composite::Clear => 2 + frame_film as usize * 2 + two,
            Composite::Shadowed => 6 + frame_film as usize * 2 + two,
        };
        Self {
            realtime: schedule,
            primary: frame_film as usize,
            post,
        }
    }
}
