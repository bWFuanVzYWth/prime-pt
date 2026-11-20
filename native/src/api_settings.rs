//! 宿主设置提交；能力、历史失效与保存策略仍由同一Engine所有者建立。

use super::*;

/// 请求渲染模式；0/1 分别为实时/离线，均使用普通 PT。
/// 离线等待首个有效帧，冻结后拒绝实时源修改；退出恢复实时更新。
///
/// # Safety
/// 宿主已确认此前全部 GPU 命令完成，调用期间独占会话及队列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_set_rendering(id: u64, mode: u32) -> i32 {
    guarded(-1, || {
        let mode = prime_scene::RenderMode::try_from(mode)?;
        with_engine(id, |engine| {
            engine.gpu.set_rendering(mode)?;
            engine.rendering.select(mode);
            Ok(())
        })
    })
}

/// 设置原生单帧诊断，参数校验后发布。
/// 诊断开关为 0 或 1；离线模式忽略该偏好。
/// # Safety
/// 宿主已完成旧GPU命令，并独占会话及队列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_set_render_tuning(id: u64, diagnostic: u32) -> i32 {
    guarded(-1, || {
        if diagnostic > 1 {
            return Err(Error::Input("诊断开关须为0或1"));
        }
        let tuning = prime_scene::RenderTuning::new(diagnostic != 0);
        with_engine(id, |engine| {
            engine.gpu.set_render_tuning(tuning)?;
            Ok(())
        })
    })
}

/// 设置 SER 偏好；enabled=0 关闭，enabled=1 在已启用的设备能力上开启，其余值拒绝。
/// 不支持时保留普通程序；成功返回 0。失败返回 -1，保留旧程序与历史。
/// 切换只创建对应管线/SBT，不重新编译 callable、分析 atlas 或复制场景/图像；
/// 旧 Flight 维持提交资源的生命周期，PT/RR 采样历史不变。
///
/// # Safety
/// 宿主已确认该会话全部旧命令完成，设备仍活动，并独占会话和 graphics queue。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_set_ser(id: u64, enabled: u32) -> i32 {
    guarded(-1, || {
        if enabled > 1 {
            return Err(Error::Input("SER 必须为 0 或 1"));
        }
        with_engine(id, |engine| {
            engine.gpu.set_ser(enabled != 0)?;
            Ok(())
        })
    })
}

/// 切换世界域，取消旧捕获并清空场景与重建历史；atlas 保留。
///
/// # Safety
/// 宿主已提交并确认此会话全部旧命令完成；调用线程独占会话与 graphics queue。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_reset(id: u64) -> i32 {
    guarded(-1, || {
        with_engine(id, |engine| {
            engine.rendering.changed();
            engine.scene.clear();
            engine.tint_world = None;
            engine.tint_pending = None;
            engine.tint_radius = None;
            engine.tint_columns.clear();
            engine.tint_invalidate_all = false;
            engine.gpu.clear_world()?;
            Ok(())
        })
    })
}

/// 选择场景更新的确定性工作额度；0平稳、1均衡、2吞吐，其他值拒绝。
/// 只影响后续建网和发布，捕获上限由宿主通过 [`prime_plan`] 的容量传入。
#[unsafe(no_mangle)]
pub extern "C" fn prime_set_update_budget(id: u64, preset: u32) -> i32 {
    guarded(-1, || {
        let (mesh_slabs, upload_mib) = match preset {
            0 => (48, 32),
            1 => (96, 64),
            2 => (128, 128),
            _ => return Err(Error::Input("未知场景更新档位")),
        };
        with_engine(id, |engine| {
            engine
                .gpu
                .set_terrain_upload_budget(upload_mib * 1024 * 1024)?;
            engine.scene.set_work_budget(scene::WorkBudget {
                mesh_slabs,
                ..engine.scene.work_budget()
            });
            Ok(())
        })
    })
}

/// 更新会话的观测纬度（−90..=90°）和太阳黄经（0..=359°）。
/// 成功返回 0。句柄或观测参数无效时返回 -1，保持原轨迹。
/// 纯 CPU 设置，与 GPU 完成和世界切换独立。
#[unsafe(no_mangle)]
pub extern "C" fn prime_set_solar_orbit(id: u64, latitude: i32, ecliptic_longitude: u32) -> i32 {
    guarded(-1, || {
        let orbit = SolarOrbit::from_degrees(latitude, ecliptic_longitude)?;
        with_live_engine(id, |engine| {
            engine.solar_orbit = orbit;
            Ok(())
        })
    })
}

/// 设置自动曝光强度、DRT 饱和补偿、色相补偿和压缩起点，均为归一化比例。
/// 对应范围为 0..=1、0..=0.5、0..=1、0..=0.99。
/// 成功返回 0。会话或参数无效时返回 -1，保留原值。调用只修改 CPU 设置。
#[unsafe(no_mangle)]
pub extern "C" fn prime_set_display_options(
    id: u64,
    auto_exposure: f32,
    saturation: f32,
    hue: f32,
    compression_start: f32,
) -> i32 {
    guarded(-1, || {
        with_engine(id, |engine| {
            engine
                .gpu
                .set_display_options(auto_exposure, saturation, hue, compression_start)?;
            Ok(())
        })
    })
}

/// 设置镜头泛光，0 关闭、3 相机光圈、4 瞳孔、5 快速圆形，旧值 1、2 分别映射到 3、4。实时和离线视图共享设置，下一次呈现生效。
/// 成功返回 0，未知光瞳或无效会话返回 −1。
#[unsafe(no_mangle)]
pub extern "C" fn prime_set_lens_bloom(id: u64, mode: u32) -> i32 {
    guarded(-1, || {
        let aperture = match mode {
            0 => None,
            1 | 3 => Some(prime_renderer::Aperture::Camera.into()),
            2 | 4 => Some(prime_renderer::Aperture::Pupil.into()),
            5 => Some(prime_renderer::LensMode::FastCircular),
            _ => return Err(Error::Input("未知镜头泛光模式")),
        };
        with_engine(id, |engine| {
            engine.gpu.set_lens_bloom(aperture);
            Ok(())
        })
    })
}

/// 设置太阳强度（−8..=8 EV）与方块发光强度（−10..=10 EV）。
/// 只修改下一帧的 CPU 光照参数。成功返回 0，会话或参数无效返回 −1 并保留原值。
#[unsafe(no_mangle)]
pub extern "C" fn prime_set_lighting_options(id: u64, sun_ev: f32, block_emission_ev: f32) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.gpu.set_lighting_options(sun_ev, block_emission_ev)?;
            Ok(())
        })
    })
}

/// 更新真实相机介质：0空气、1水、2岩浆，不触发资源准备或RR全局重置。
#[unsafe(no_mangle)]
pub extern "C" fn prime_camera_medium(id: u64, kind: u32) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine
                .gpu
                .set_initial_medium(engine.catalog.camera_medium(kind)?)?;
            Ok(())
        })
    })
}
