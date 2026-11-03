//! 世界位姿与 Streamline 常量；相机数学和 Halton 序列由引擎维护。
//! 接受的前帧保存绝对位置，每帧先转到当前anchor，再生成shader与SDK的前后投影。
//! 相机运动、FOV和anchor变化不自行宣布整个历史域失效，重置事件由GPU所有者决定。

use crate::{scene::Camera, streamline};
use camera::{Frame, Jitter, Projection, Relative};

const NEAR: f32 = 0.01;
const FAR: f32 = 1_000_000.0;

/// 已校验相机及固定 SDK 深度范围；不持有 SDK 或图像资源。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CameraFrame {
    world: Frame,
    projection: Projection,
    anchor: [f64; 3],
}

impl CameraFrame {
    /// 从当前anchor下的追踪相机保存世界位姿；不丢失旧帧使用的投影。
    pub(crate) fn new(camera: &Camera, anchor: [f64; 3]) -> Result<Self, camera::Error> {
        if !camera.origin.iter().all(|value| value.is_finite())
            || !anchor.iter().all(|value| value.is_finite())
        {
            return Err(camera::Error::Position);
        }
        camera.validate().map_err(|_| camera::Error::Basis)?;
        let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
        let world = Frame::from_scaled(
            std::array::from_fn(|axis| anchor[axis] + f64::from(camera.origin[axis])),
            xyz(camera.forward),
            xyz(camera.right),
            xyz(camera.up),
        )?;
        let relative = world.relative(anchor)?;
        let projection = Projection::new(relative.tangent(), relative.aspect(), NEAR, FAR)?;
        Ok(Self {
            world,
            projection,
            anchor,
        })
    }

    fn relative(self, anchor: [f64; 3]) -> Result<Relative, camera::Error> {
        self.world.relative(anchor)
    }

    /// 与 `geometry::Camera` 的四个float4一致；前帧也必须使用本帧anchor，不含jitter。
    pub(crate) fn camera_words(self, anchor: [f64; 3]) -> Result<[f32; 16], camera::Error> {
        Ok(self.relative(anchor)?.words())
    }

    /// SDK jitter 为实际输入像素中心偏移的负值；reset不引用失效历史。
    /// 前后位姿都使用构造本帧时的anchor，避免调用者混用旧帧坐标系。
    pub(crate) fn frame(
        self,
        command: u64,
        index: u32,
        reset: bool,
        jitter: [f32; 2],
        previous: &Self,
        images: [streamline::Resource; 9],
    ) -> Result<streamline::Frame, camera::Error> {
        let previous = if reset { self } else { *previous };
        let current_camera = self.relative(self.anchor)?;
        let previous_camera = previous.relative(self.anchor)?;
        Ok(streamline::Frame {
            command,
            index,
            reset: u32::from(reset),
            jitter: jitter.map(|v| -v),
            near_plane: self.projection.near(),
            far_plane: self.projection.far(),
            fov: 2.0 * current_camera.tangent().atan(),
            aspect: current_camera.aspect(),
            position: current_camera.position(),
            up: current_camera.up(),
            right: current_camera.right(),
            forward: current_camera.forward(),
            world_to_view: current_camera.world_to_view().to_array(),
            view_to_world: current_camera.view_to_world().to_array(),
            view_to_clip: self.projection.view_to_clip().to_array(),
            clip_to_view: self.projection.clip_to_view().to_array(),
            clip_to_previous: current_camera
                .clip_to(self.projection, previous_camera, previous.projection)
                .to_array(),
            previous_to_clip: previous_camera
                .clip_to(previous.projection, current_camera, self.projection)
                .to_array(),
            images,
        })
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
        let camera = CameraFrame::new(&camera(), [0.; 3]).unwrap();
        let jitter = jitter(3, 960, 1920).unwrap();
        let frame = camera
            .frame(
                1,
                3,
                false,
                jitter,
                &camera,
                [streamline::Resource::default(); 9],
            )
            .unwrap();
        assert_eq!(frame.jitter, jitter.map(|v| -v));
        assert_eq!(
            frame.view_to_clip,
            camera.projection.view_to_clip().to_array()
        );
        assert_eq!(frame.aspect, 2.0);
    }

    #[test]
    fn reset_uses_current_camera_for_both_clip_transforms() {
        let current = CameraFrame::new(&camera(), [0.; 3]).unwrap();
        let mut source = camera();
        source.origin[0] += 8.;
        let previous = CameraFrame::new(&source, [0.; 3]).unwrap();
        let frame = current
            .frame(
                1,
                3,
                true,
                [0.; 2],
                &previous,
                [streamline::Resource::default(); 9],
            )
            .unwrap();
        for matrix in [frame.clip_to_previous, frame.previous_to_clip] {
            for (i, value) in matrix.into_iter().enumerate() {
                assert!((value - f32::from(i / 4 == i % 4)).abs() < 3e-4);
            }
        }
    }

    #[test]
    fn anchor_crossing_preserves_world_motion_in_shader_and_sdk() {
        for boundary in [-64.0, 64.0] {
            let previous_anchor = [boundary - 64.0, 0.0, 0.0];
            let current_anchor = [boundary, 0.0, 0.0];
            let mut source = Camera {
                origin: [63.75, 0.0, 0.0, 0.0],
                forward: [0.0, 0.0, 1.0, 0.0],
                right: [1.0, 0.0, 0.0, 0.0],
                up: [0.0, 1.0, 0.0, 0.0],
            };
            let previous = CameraFrame::new(&source, previous_anchor).unwrap();
            source.origin[0] = 0.25;
            let current = CameraFrame::new(&source, current_anchor).unwrap();
            assert_eq!(previous.camera_words(current_anchor).unwrap()[0], -0.25);
            let frame = current
                .frame(
                    1,
                    1,
                    false,
                    [0.0; 2],
                    &previous,
                    [streamline::Resource::default(); 9],
                )
                .unwrap();
            assert_eq!(frame.reset, 0);
            assert_eq!(frame.position, [0.25, 0.0, 0.0]);
            let current_clip = current
                .projection
                .view_to_clip()
                .transform([0.0, 0.0, 10.0, 1.0]);
            let previous_clip = transform(frame.clip_to_previous, current_clip);
            // 移动0.5m、前方10m、tan(FOV/2)=1，上一帧NDC x应为0.05。
            assert!((previous_clip[0] / previous_clip[3] - 0.05).abs() < 1e-5);
        }
    }

    #[test]
    fn projection_change_uses_the_accepted_previous_projection() {
        let mut source = Camera {
            origin: [0.0; 4],
            forward: [0.0, 0.0, 1.0, 0.0],
            right: [1.0, 0.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0, 0.0],
        };
        let previous = CameraFrame::new(&source, [0.; 3]).unwrap();
        source.right[0] = 2.0;
        source.up[1] = 2.0;
        let current = CameraFrame::new(&source, [0.; 3]).unwrap();
        let frame = current
            .frame(
                1,
                1,
                false,
                [0.0; 2],
                &previous,
                [streamline::Resource::default(); 9],
            )
            .unwrap();
        let current_clip = current
            .projection
            .view_to_clip()
            .transform([1.0, 0.0, 10.0, 1.0]);
        let previous_clip = transform(frame.clip_to_previous, current_clip);
        assert_eq!(frame.reset, 0);
        assert!((previous_clip[0] / previous_clip[3] - 0.1).abs() < 1e-5);
        assert!((current_clip[0] / current_clip[3] - 0.05).abs() < 1e-5);
    }

    fn transform(matrix: [f32; 16], point: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|column| {
            (0..4)
                .map(|row| point[row] * matrix[row * 4 + column])
                .sum()
        })
    }
}
