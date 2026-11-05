//! 离线快照只保存常量帧输入；场景数据仍由原所有者持有，不复制世界或 GPU 资源。

use crate::{Environment, Error, WorldCamera};
use prime_scene::RenderMode;

#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub camera: WorldCamera,
    pub environment: Environment,
    pub bounces: u32,
}

#[derive(Default)]
pub(super) struct State {
    mode: RenderMode,
    last: Option<Frame>,
}

impl State {
    pub const fn mode(&self) -> RenderMode {
        self.mode
    }
    pub const fn frozen(&self) -> bool {
        matches!(self.mode, RenderMode::Offline) && self.last.is_some()
    }
    pub fn select(&mut self, mode: RenderMode) {
        self.mode = mode;
    }
    pub fn require_live(&self) -> Result<(), Error> {
        if self.frozen() {
            return Err(Error::Input("离线快照期间拒绝实时源和光照修改"));
        }
        Ok(())
    }
    pub fn changed(&mut self) {
        self.last = None;
    }
    pub fn choose(&self, incoming: Frame) -> Frame {
        if self.frozen() {
            let mut frame = self.last.expect("冻结具有完整已接受帧");
            frame.environment.exposure_ev = incoming.environment.exposure_ev;
            frame
        } else {
            incoming
        }
    }
    pub fn accepted(&mut self, frame: Frame) {
        self.last = Some(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(x: f64) -> Frame {
        Frame {
            camera: WorldCamera {
                origin: [x, 1., 2.],
                forward: [0., 0., 1., 0.],
                right: [1., 0., 0., 0.],
                up: [0., 1., 0., 0.],
            },
            environment: Environment::default(),
            bounces: 12,
        }
    }

    #[test]
    fn offline_waits_for_the_first_accepted_frame() {
        let mut state = State::default();
        state.select(RenderMode::Offline);
        assert!(!state.frozen());
        assert!(state.require_live().is_ok());
        let first = state.choose(frame(3.));
        state.accepted(first);
        assert!(state.frozen());
        assert!(state.require_live().is_err());
    }

    #[test]
    fn frozen_frame_only_accepts_display_exposure() {
        let mut state = State::default();
        state.accepted(frame(3.));
        state.select(RenderMode::Offline);
        let mut incoming = frame(100.);
        incoming.environment.sun_direction = [1., 0., 0.];
        incoming.environment.exposure_ev = 2.;
        incoming.bounces = 64;
        let frozen = state.choose(incoming);
        assert_eq!(frozen.camera.origin, [3., 1., 2.]);
        assert_eq!(
            frozen.environment.sun_direction,
            Environment::default().sun_direction
        );
        assert_eq!((frozen.bounces, frozen.environment.exposure_ev), (12, 2.));
    }

    #[test]
    fn world_change_discards_a_pending_offline_snapshot() {
        let mut state = State::default();
        state.accepted(frame(3.));
        state.select(RenderMode::Offline);
        state.changed();
        assert!(!state.frozen());
        assert_eq!(state.choose(frame(9.)).camera.origin[0], 9.);
        state.select(RenderMode::Realtime);
        assert!(state.require_live().is_ok());
    }
}
