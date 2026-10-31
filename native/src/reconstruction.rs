//! Mod 的历史策略与 Streamline 常量；相机数学和 Halton 序列由引擎维护。
//! 历史仅在成功录制后由 renderer 接受；尺寸/场景改变和 SDK 状态由上层决定 reset。

use crate::{scene::Camera, streamline};
use camera::{Frame, Jitter, Projection, Relative};

const NEAR: f32 = 0.01;
const FAR: f32 = 1_000_000.0;

/// 已校验相机及固定 SDK 深度范围；不持有 SDK 或图像资源。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CameraFrame {
    camera: Relative,
    projection: Projection,
}

impl CameraFrame {
    /// 将 Mod 半视锥输入交给引擎归一化；世界已由场景转换到相同锚点。
    pub(crate) fn new(camera: &Camera) -> Result<Self, crate::Error> {
        camera.validate()?;
        let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
        let camera = Frame::from_scaled(
            xyz(camera.origin).map(f64::from),
            xyz(camera.forward),
            xyz(camera.right),
            xyz(camera.up),
        )
        .and_then(|frame| frame.relative([0.0; 3]))
        .map_err(crate::gpu::Error::from)?;
        let projection = Projection::new(camera.tangent(), camera.aspect(), NEAR, FAR)
            .map_err(crate::gpu::Error::from)?;
        Ok(Self { camera, projection })
    }

    /// 与 `geometry::Camera` 的四个 float4 一致，不含 jitter。
    pub(crate) const fn camera_words(self) -> [f32; 16] {
        self.camera.words()
    }

    /// Mod 的切镜策略；容忍归一化舍入，普通局部移动保留历史。
    pub(crate) fn history_compatible(self, previous: &Self) -> bool {
        let same_projection = |a: f32, b: f32| (a - b).abs() <= a.abs().max(b.abs()) * 1e-4;
        let displacement: [f32; 3] =
            std::array::from_fn(|i| self.camera.position()[i] - previous.camera.position()[i]);
        let distance_squared = displacement.iter().map(|v| v * v).sum::<f32>();
        let direction_dot = self
            .camera
            .forward()
            .into_iter()
            .zip(previous.camera.forward())
            .map(|(a, b)| a * b)
            .sum::<f32>();
        same_projection(self.camera.tangent(), previous.camera.tangent())
            && same_projection(self.camera.aspect(), previous.camera.aspect())
            && distance_squared <= 16.0 * 16.0
            && direction_dot >= 0.5
    }

    /// SDK jitter 为实际输入像素中心偏移的负值；reset 不引用失效历史。
    pub(crate) fn frame(
        self,
        command: u64,
        index: u32,
        reset: bool,
        jitter: [f32; 2],
        previous: &Self,
        images: [streamline::Resource; 9],
    ) -> streamline::Frame {
        let previous = if reset { self } else { *previous };
        streamline::Frame {
            command,
            index,
            reset: u32::from(reset),
            jitter: jitter.map(|v| -v),
            near_plane: self.projection.near(),
            far_plane: self.projection.far(),
            fov: 2.0 * self.camera.tangent().atan(),
            aspect: self.camera.aspect(),
            position: self.camera.position(),
            up: self.camera.up(),
            right: self.camera.right(),
            forward: self.camera.forward(),
            world_to_view: self.camera.world_to_view().to_array(),
            view_to_world: self.camera.view_to_world().to_array(),
            view_to_clip: self.projection.view_to_clip().to_array(),
            clip_to_view: self.projection.clip_to_view().to_array(),
            clip_to_previous: self
                .camera
                .clip_to(self.projection, previous.camera, previous.projection)
                .to_array(),
            previous_to_clip: previous
                .camera
                .clip_to(previous.projection, self.camera, self.projection)
                .to_array(),
            images,
        }
    }
}

/// Mod 的输入/输出宽度选择引擎 Halton 周期；尺寸错误显式上抛。
pub(crate) const fn jitter(
    index: u32,
    input_width: u32,
    output_width: u32,
) -> Result<[f32; 2], camera::Error> {
    match Jitter::new(input_width, output_width) {
        Ok(sequence) => Ok(sequence.sample(index)),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            origin: [3., 7., 2., 0.],
            forward: [0., 0., -2., 0.],
            right: [3., 0., 0., 0.],
            up: [0., 1.5, 0., 0.],
        }
    }

    #[test]
    fn sdk_receives_negative_input_pixel_jitter_and_unjittered_projection() {
        let camera = CameraFrame::new(&camera()).unwrap();
        let jitter = jitter(3, 960, 1920).unwrap();
        let frame = camera.frame(
            1,
            3,
            false,
            jitter,
            &camera,
            [streamline::Resource::default(); 9],
        );
        assert_eq!(frame.jitter, jitter.map(|v| -v));
        assert_eq!(
            frame.view_to_clip,
            camera.projection.view_to_clip().to_array()
        );
        assert_eq!(frame.aspect, 2.0);
    }

    #[test]
    fn reset_uses_current_camera_for_both_clip_transforms() {
        let current = CameraFrame::new(&camera()).unwrap();
        let mut source = camera();
        source.origin[0] += 8.;
        let previous = CameraFrame::new(&source).unwrap();
        let frame = current.frame(
            1,
            3,
            true,
            [0.; 2],
            &previous,
            [streamline::Resource::default(); 9],
        );
        for matrix in [frame.clip_to_previous, frame.previous_to_clip] {
            for (i, value) in matrix.into_iter().enumerate() {
                assert!((value - f32::from(i / 4 == i % 4)).abs() < 3e-4);
            }
        }
    }

    #[test]
    fn local_camera_motion_keeps_mod_history() {
        let previous = CameraFrame::new(&camera()).unwrap();
        let mut source = camera();
        source.origin[0] += 2.;
        let (sine, cosine) = 0.1f32.sin_cos();
        source.forward = [2. * sine, 0., -2. * cosine, 0.];
        source.right = [3. * cosine, 0., 3. * sine, 0.];
        assert!(
            CameraFrame::new(&source)
                .unwrap()
                .history_compatible(&previous)
        );
    }

    #[test]
    fn mod_projection_changes_and_camera_cuts_drop_history() {
        let previous = CameraFrame::new(&camera()).unwrap();
        let mut fov = camera();
        fov.up[1] *= 1.1;
        let mut teleport = camera();
        teleport.origin[0] += 100.;
        let mut reverse = camera();
        reverse.forward[2] *= -1.;
        reverse.right[0] *= -1.;
        for source in [fov, teleport, reverse] {
            assert!(
                !CameraFrame::new(&source)
                    .unwrap()
                    .history_compatible(&previous)
            );
        }
    }
}
