//! 帧输入的有限执行计划；程序、采样身份与后处理选择由同一所有者建立。
use super::Error;
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
    pub const fn new(
        realtime: bool,
        samples: u32,
        split: bool,
        frame_film: bool,
        composite: Composite,
    ) -> Result<Self, Error> {
        let schedule = if realtime {
            Some(match samples {
                1 => RealtimeSchedule::One,
                2 if split => RealtimeSchedule::SplitTwo,
                2 => RealtimeSchedule::MergedTwo,
                _ => {
                    return Err(Error::Invalid(
                        "实时PT只接受性能（1 spp）或质量（2 spp）策略",
                    ));
                }
            })
        } else {
            None
        };
        let two = (realtime && samples == 2) as usize;
        let post = match composite {
            Composite::None => two,
            Composite::Clear => 2 + frame_film as usize * 2 + two,
            Composite::Shadowed => 6 + frame_film as usize * 2 + two,
        };
        Ok(Self {
            realtime: schedule,
            primary: frame_film as usize,
            post,
        })
    }
}
