//! Minecraft 光照源到通用渲染环境的帧输入边界。

use crate::{Environment, Error};
use simple_engine::atmosphere::SolarOrbit;

/// 宿主的 16 B 环境包，依次为太阳时角（f32 度）、曝光（f32 EV）、世界海平面（f32 米）
/// 和天空开关（u32，0 禁用或 1 启用）。海平面由宿主给出，太阳方向由准备好的轨迹计算。
#[derive(Clone, Copy)]
#[repr(C)]
pub struct FrameEnvironment {
    sun_angle_degrees: f32,
    exposure_ev: f32,
    sea_level: f32,
    sky: u32,
}

impl FrameEnvironment {
    pub(crate) fn resolve(self, solar: SolarOrbit, anchor_y: f64) -> Result<Environment, Error> {
        let sun_direction = match self.sky {
            0 => [0.; 3],
            1 => solar.direction(self.sun_angle_degrees.to_radians())?,
            _ => return Err(Error::Input("天空开关须为 0 或 1")),
        };
        let mut environment = Environment {
            sun_direction,
            exposure_ev: self.exposure_ev,
            sea_level: self.sea_level,
        }
        .validated()?;
        environment.sea_level = (f64::from(environment.sea_level) - anchor_y) as f32;
        Ok(environment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_host_frame_uses_the_session_solar_orbit() -> Result<(), Error> {
        let host = FrameEnvironment {
            sun_angle_degrees: 0.,
            exposure_ev: 1.25,
            sea_level: 63.,
            sky: 1,
        };
        let orbit = SolarOrbit::from_degrees(60, 0)?;
        let resolved = host.resolve(orbit, 64.)?;
        assert!((resolved.sun_direction[1] - 0.5).abs() < 1e-6);
        assert_eq!(resolved.exposure_ev, 1.25);
        assert_eq!(resolved.sea_level, -1.);
        assert!(
            !FrameEnvironment { sky: 0, ..host }
                .resolve(orbit, 64.)?
                .enabled()
        );
        Ok(())
    }
}
