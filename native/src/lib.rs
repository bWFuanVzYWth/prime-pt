//! Minecraft 26.3 的路径追踪宿主适配与原生会话入口。
//!
//! # 组件与数据流
//!
//! Java 绑定宿主类型，转发资源字典、区块源、变化与设备句柄。
//! [`prime_minecraft`] 校验资源世代并把宿主数据编译为几何及材质输入；
//! [`prime_world::Scene`] 持有需求窗口、捕获票据、预算与区块替换候选。
//! [`prime_scene`] 定义宿主无关的几何、材质身份与相机视图，
//! [`prime_renderer::Gpu`] 准备并提交 GPU 资源、积分和呈现。
//! 引擎提供通用算法及设备接入，Minecraft 的源数据解释与设置策略留在本项目。
//!
//! # 宿主调用边界
//!
//! Java 在渲染线程同步借出源包、atlas 与宿主 Vulkan 句柄；Rust 拥有 mesh、
//! 工作池、BLAS/TLAS、SBT 和 pipeline。操作 GPU 所有者前，宿主必须提交并等待
//! 前次 native 命令完成，且独占 graphics queue。纯 CPU 场景阶段可在等待前推进。
//! 句柄不会转移设备所有权。
//! 源包只在调用内借用；palette/资源字典解码成自持输入，section编译及tint续接分阶段发布。
//! 一个64m region合并4³个section；缺源section保留旧片段，已完成片段可发布。
//! 使用 [`Quad`] 的直接寻址属性 ABI：任意四角、固定012/023、三角面退化为quad，
//! 不要求轴对齐。PT 的主查询使用 AS position fetch；当帧首面缓存另为照明和导引
//! 保存局部四角和 region 键，其所有者与释放边界见渲染 crate。
//! 宿主必须启用 [KHR position fetch] 扩展和 feature，无回退；能力边界见 [`Quad`]。
//!
//! [KHR position fetch]: https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_ray_tracing_position_fetch.html

#![recursion_limit = "256"]

use simple_engine::{atmosphere, frame_loop};

mod api_session;
pub use api_session::{
    prime_create, prime_create_options, prime_destroy, prime_error, prime_report_device_loss,
    prime_set_runtime_root, prime_streamline_bootstrap, prime_streamline_shutdown,
};

mod api_settings;
pub use api_settings::{
    prime_camera_medium, prime_reset, prime_set_display_options, prime_set_lens_bloom,
    prime_set_lighting_options, prime_set_render_tuning, prime_set_rendering, prime_set_ser,
    prime_set_solar_orbit, prime_set_update_budget,
};

mod api_capture;
pub use api_capture::{
    prime_batch, prime_fail_capture, prime_invalidate_all, prime_invalidate_batch,
    prime_invalidate_section, prime_plan, prime_section_published,
};

mod api_resources;
pub use api_resources::{prime_atlas, prime_sources};

mod api_tint;
pub use api_tint::{
    prime_biome_requests, prime_biome_samples, prime_biome_world, prime_invalidate_tints,
    prime_tint_begin, prime_tint_requests, prime_tints,
};

mod api_render;
pub use api_render::{prime_draw, prime_profile};

mod capture;
mod changes;
mod diagnostics;
mod frame_environment;
mod rendering;
mod runtime;
use prime_renderer as gpu;
use prime_renderer::streamline;
use prime_world as scene;
use simple_engine::atmosphere::SolarOrbit;

pub use frame_environment::FrameEnvironment;
pub use prime_scene::{Camera, Environment, Material, Mesh, Quad, WorldCamera};

use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// 输入、工作池或 Vulkan 失败；底层错误通过 source 保留。
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// 源协议、句柄或参数不符合约定。
    #[error("{0}")]
    Input(&'static str),
    /// Vulkan 加载、资源或命令失败。
    #[error(transparent)]
    Gpu(#[from] gpu::Error),
    /// 发行版原生工具或 shader 源路径无法访问。
    #[error("发行资源 {path}：{source}")]
    RuntimePath {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// Streamline 初始化或全局退役失败。
    #[error(transparent)]
    Streamline(#[from] streamline::Error),
    /// 区块所有者或源包失败。
    #[error(transparent)]
    World(#[from] prime_world::Error),
    /// Minecraft源资源、压缩palette或材质准备失败。
    #[error(transparent)]
    Minecraft(#[from] prime_minecraft::Error),
    /// 通用场景输入失败。
    #[error(transparent)]
    Scene(#[from] prime_scene::Error),
    /// 帧统计保留容量增长失败。
    #[error(transparent)]
    Diagnostics(#[from] frame_loop::timing::FrameStatsError),
    /// 观测者、太阳黄经或时角输入无效。
    #[error(transparent)]
    Solar(#[from] atmosphere::SolarOrbitError),
}

struct Engine {
    capture: Option<capture::Capture>,
    scene: scene::Scene,
    attribute_limited_resident_bytes: u64,
    gpu: gpu::Gpu,
    atlas_ready: bool,
    catalog: prime_minecraft::Catalog,
    pending_albedo: Option<([u32; 2], Vec<u8>)>,
    tint_world: Option<prime_minecraft::tint::TintWorld>,
    tint_pending: Option<TintContinuation>,
    tint_radius: Option<u32>,
    tint_columns: std::collections::HashSet<(i32, i32)>,
    tint_invalidate_all: bool,
    rendering: rendering::State,
    solar_orbit: SolarOrbit,
    profile: diagnostics::Profile,
}

struct TintContinuation {
    plan: prime_minecraft::tint::TintPlan,
    ids: Option<Vec<u32>>,
    host: Option<Vec<u32>>,
}

impl Engine {
    /// 视距从 16 到 32 时，每格增加 64 MiB 逻辑常驻额度，最高 2 GiB，受 GPU 属性容量约束。
    /// 会话内只提高上限，窗口收缩按实际退出的 region 回收资源。
    fn grow_view_budget(&mut self, view_distance: u32) -> Result<(), Error> {
        const MIB: usize = 1024 * 1024;
        let mut budget = self.scene.budget();
        let requested = (1024 + view_distance.saturating_sub(16).min(16) as usize * 64) * MIB;
        if requested <= budget.resident_bytes {
            return Ok(());
        }
        let resident = (requested as u64).min(self.attribute_limited_resident_bytes) as usize;
        if resident > budget.resident_bytes {
            budget.resident_bytes = resident;
            self.scene.set_budget(budget)?;
        }
        Ok(())
    }

    fn retire_tint_sources(&mut self) {
        for key in self.scene.drain_retired_sources() {
            self.tint_pending = None;
            if let Some(world) = &mut self.tint_world {
                world.forget_section(key);
            }
        }
    }
    fn finish_tints(&mut self) -> Result<(), Error> {
        let Some(pending) = self.tint_pending.as_ref() else {
            return Ok(());
        };
        if pending.ids.is_none() || pending.host.is_none() {
            return Ok(());
        }
        let pending = self.tint_pending.take().expect("完整响应阶段");
        let resources = self
            .catalog
            .tints()
            .ok_or(Error::Input("缺少同世代tint资源"))?;
        let colors = self
            .tint_world
            .as_mut()
            .ok_or(Error::Input("biome世界源尚未准备"))?
            .finish(
                pending.plan,
                resources,
                &pending.ids.expect("已校验biome阶段"),
                &pending.host.expect("已校验host阶段"),
            )?;
        self.scene.apply_tints(&colors)?;
        Ok(())
    }
}

static ENGINES: OnceLock<Mutex<BTreeMap<u64, Engine>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn report(error: impl std::fmt::Display) {
    let message = error.to_string().replace('\0', " ");
    LAST_ERROR.with(|slot| *slot.borrow_mut() = CString::new(message).unwrap());
}

fn guarded<T: Copy>(failure: T, action: impl FnOnce() -> Result<T, Error>) -> T {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            report(error);
            failure
        }
        Err(_) => {
            report("native panic；请重启实验，当前会话不可继续");
            failure
        }
    }
}

fn with_engine(
    id: u64,
    action: impl FnOnce(&mut Engine) -> Result<(), Error>,
) -> Result<i32, Error> {
    let mut engines = ENGINES
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| Error::Input("native 会话锁已损坏"))?;
    let engine = engines
        .get_mut(&id)
        .ok_or(Error::Input("无效 native 句柄"))?;
    action(engine)?;
    Ok(0)
}

fn with_live_engine(
    id: u64,
    action: impl FnOnce(&mut Engine) -> Result<(), Error>,
) -> Result<i32, Error> {
    with_engine(id, |engine| {
        engine.rendering.require_live()?;
        action(engine)?;
        engine.rendering.changed();
        Ok(())
    })
}

fn elapsed_ns(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
