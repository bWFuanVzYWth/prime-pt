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
//! 不要求轴对齐。普通 PT 的主查询只用 AS position fetch；ReSTIR 另按历史交点恢复需求
//! 保存局部四角和 region 键，其所有者与释放边界见渲染 crate。
//! 宿主必须启用 [KHR position fetch] 扩展和 feature，无回退；能力边界见 [`Quad`]。
//!
//! [KHR position fetch]: https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_ray_tracing_position_fetch.html

#![recursion_limit = "256"]

use simple_engine::{atmosphere, frame_loop};
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

/// 请求渲染模式和实时积分器；0/1 分别为实时/离线和普通 PT/ReSTIR PT。
/// 离线等待首个有效帧，冻结后拒绝实时源修改；退出保留实时积分器偏好。
///
/// # Safety
/// 宿主已确认此前全部 GPU 命令完成，调用期间独占会话及队列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_set_rendering(id: u64, mode: u32, renderer: u32) -> i32 {
    guarded(-1, || {
        let mode = prime_scene::RenderMode::try_from(mode)?;
        let renderer = prime_scene::RealtimeRenderer::try_from(renderer)?;
        with_engine(id, |engine| {
            engine.gpu.set_rendering(mode, renderer)?;
            engine.rendering.select(mode);
            Ok(())
        })
    })
}

/// 设置历史M cap、固定fresh概率、原生诊断和ReSTCV；参数统一校验后发布。
/// 诊断/CV均为0或1；CV只影响ReSTIR颜色历史，不清路径、RR或普通PT历史。
/// # Safety
/// 宿主已完成旧GPU命令，并独占会话及队列。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_set_render_tuning(
    id: u64,
    cap: u32,
    probability: f32,
    diagnostic: u32,
    restir_cv: u32,
) -> i32 {
    guarded(-1, || {
        if diagnostic > 1 || restir_cv > 1 {
            return Err(Error::Input("诊断和ReSTCV开关须为0或1"));
        }
        let tuning =
            prime_scene::RenderTuning::new(cap, probability, diagnostic != 0, restir_cv != 0)?;
        with_engine(id, |engine| {
            engine.gpu.set_render_tuning(tuning)?;
            Ok(())
        })
    })
}

/// 返回当前线程最后一次失败信息；UTF-8 指针有效至该线程下一次失败。
#[unsafe(no_mangle)]
pub extern "C" fn prime_error() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ptr())
}

/// 在首次尝试创建会话前配置发行资源根目录。路径为 UTF-8 绝对路径，不含终止符。
/// 成功返回 0，重复指定同一目录也成功。配置固定到 DLL 卸载，失败返回 -1 并保留诊断。
/// 宿主须保持该目录下的编译器和 shader 源有效，缓存写入同根 `cache/`。
///
/// # Safety
/// 调用者借出 `len` 字节的有效只读连续内存，直至调用返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_set_runtime_root(data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        // Windows 长路径最多 32767 个 UTF-16 单元；四倍为 UTF-8 的保守上界。
        if data.is_null() || !(1..=4 * 32767).contains(&len) {
            return Err(Error::Input("发行资源路径指针或长度无效"));
        }
        // SAFETY: 内存有效性由宿主保证，长度已限制在 isize::MAX 内。
        let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
        let path =
            std::str::from_utf8(bytes).map_err(|_| Error::Input("发行资源路径不是 UTF-8"))?;
        if path.contains('\0') {
            return Err(Error::Input("发行资源路径包含 NUL"));
        }
        runtime::configure(std::path::Path::new(path))?;
        Ok(0)
    })
}

/// 在宿主创建 Vulkan instance/device 前安装 Streamline；失败返回 -1 并保留来源。
#[unsafe(no_mangle)]
pub extern "C" fn prime_streamline_bootstrap() -> i32 {
    guarded(-1, || {
        streamline::bootstrap()?;
        Ok(0)
    })
}

/// 所有 native 会话与 GPU 工作完成后，在宿主销毁设备前退役 Streamline。
#[unsafe(no_mangle)]
pub extern "C" fn prime_streamline_shutdown() -> i32 {
    guarded(-1, || {
        streamline::shutdown()?;
        Ok(0)
    })
}

/// 借用已启用硬件 RT 的宿主设备；返回非零会话 ID，失败返回 0。
///
/// # Safety
/// Vulkan 实例和物理设备须支持 1.4；句柄来自同一活动设备，queue 对应 family/index，支持 graphics/compute。
/// 设备须启用 subgroupSizeControl（compute可请求32-lane）、deviceAddressCommands、synchronization2、bufferDeviceAddress、accelerationStructure、rayTracingPipeline、rayQuery、
/// rayTracingPositionFetch 及其扩展；BLAS 必须使用 ALLOW_DATA_ACCESS，不支持无 position fetch 的设备回退。
/// K1 图像格式需要 shaderStorageImageExtendedFormats；RR 的设备能力由 Java Bootstrap 协商。
/// 宿主独占 queue 的调用线程，且不得在会话销毁前销毁设备或实例。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_create(
    instance: u64,
    physical: u64,
    device: u64,
    queue: u64,
    family: u32,
    queue_index: u32,
) -> u64 {
    // SAFETY: 旧入口保持原设备契约，不请求任何可选能力。
    unsafe { prime_create_options(instance, physical, device, queue, family, queue_index, 0) }
}

/// 带设备能力声明的会话创建；flags bit0 声明 EXT SER，bit1 关闭初始 SER，bit2 启用 NV 异步检查点，其余位拒绝。
/// bit1 只允许与 bit0 同时设置；失败返回 0。
///
/// # Safety
/// 与 [`prime_create`] 相同；bit0 为 1 时设备还须真正启用
/// `VK_EXT_ray_tracing_invocation_reorder` 与 `rayTracingInvocationReorder`。
/// 初始选择由 bit1 决定，之后可用 [`prime_set_ser`] 切换；不以物理支持替代逻辑设备启用。
/// bit2 为 1 时必须在该逻辑设备启用 `VK_NV_device_diagnostic_checkpoints`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_create_options(
    instance: u64,
    physical: u64,
    device: u64,
    queue: u64,
    family: u32,
    queue_index: u32,
    flags: u32,
) -> u64 {
    guarded(0, || {
        if flags & !7 != 0 || flags & 3 == 2 {
            return Err(Error::Input("未知设备能力 flags"));
        }
        if [instance, physical, device, queue].contains(&0) {
            return Err(Error::Input("Vulkan 句柄不能为空"));
        }
        let scene = scene::Scene::new()?;
        // SAFETY: 调用者提供同一活动设备并独占 queue，见导出接口契约。
        let mut gpu = unsafe {
            gpu::Gpu::attach_options(
                instance,
                physical,
                device,
                queue,
                family,
                queue_index,
                flags & 1 != 0,
                flags & 2 == 0,
                runtime::root(),
            )?
        };
        if flags & 4 != 0 {
            // SAFETY: bit2 是宿主逻辑设备创建边界对已启用扩展的声明。
            unsafe { gpu.enable_diagnostic_checkpoints()? };
        }
        // 属性槽按二次幂增长，额外留四分之一给空洞及过渡范围。
        let attribute_limited_resident_bytes =
            (u64::from(gpu.attribute_slot_limit()) * 3 / 4) * size_of::<Quad>() as u64;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err(Error::Input("会话 ID 耗尽"));
        }
        ENGINES
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| Error::Input("native 会话锁已损坏"))?
            .insert(
                id,
                Engine {
                    scene,
                    attribute_limited_resident_bytes,
                    gpu,
                    atlas_ready: false,
                    catalog: prime_minecraft::Catalog::default(),
                    pending_albedo: None,
                    tint_world: None,
                    tint_pending: None,
                    tint_radius: None,
                    tint_columns: std::collections::HashSet::new(),
                    tint_invalidate_all: false,
                    rendering: rendering::State::default(),
                    solar_orbit: SolarOrbit::default(),
                    profile: diagnostics::Profile::default(),
                },
            );
        Ok(id)
    })
}

/// 输出 GPU 故障异步进度；成功返回 0，诊断自身失败返回 -1 并保留来源。
/// 不等待、不提交、不销毁资源，也不吞掉宿主的原始设备丢失错误。
/// # Safety
/// 宿主已收到本会话设备的 `VK_ERROR_DEVICE_LOST`，保持设备与 queue 有效并独占访问；
/// 普通超时或其他异常不满足该前提。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_report_device_loss(id: u64) -> i32 {
    guarded(-1, || {
        with_engine(id, |engine| {
            // SAFETY: 导出接口要求宿主确认设备已经丢失。
            let driver = unsafe { engine.gpu.report_device_loss() };
            let cpu = engine
                .profile
                .report_device_loss(std::io::stderr().lock())
                .map_err(prime_renderer::Error::DiagnosticOutput);
            driver?;
            cpu?;
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

/// 释放会话自己的资源；成功返回 0，失败返回 -1。
///
/// # Safety
/// 宿主已提交并确认此会话的全部命令完成，且设备仍活动；不得并发使用该会话。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_destroy(id: u64) -> i32 {
    guarded(-1, || {
        let mut engines = ENGINES
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| Error::Input("native 会话锁已损坏"))?;
        engines
            .get_mut(&id)
            .ok_or(Error::Input("无效 native 句柄"))?
            .gpu
            .close()?;
        let engine = engines.remove(&id).expect("已确认并关闭同一独占会话");
        drop(engine);
        Ok(0)
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
        let (mesh_slabs, publish_geometry_quads, publish_attribute_quads) = match preset {
            0 => (48, 65_536, 131_072),
            1 => (96, 131_072, 262_144),
            2 => (128, 262_144, 524_288),
            _ => return Err(Error::Input("未知场景更新档位")),
        };
        with_engine(id, |engine| {
            engine.scene.set_work_budget(scene::WorkBudget {
                mesh_slabs,
                publish_geometry_quads,
                publish_attribute_quads,
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

/// 依宿主水平视距及完整世界高度推进64m LOD0窗口，返回至多capacity条待捕请求。
///
/// 请求为 little-endian 的 `id:u64, sx:i32, sy:i32, sz:i32, reserved:u32=0`，每条 24B。
/// view_distance以16m列计；高度为半开section范围。每个region合并4³个section。
/// 未回复请求在后续调用保持同一ID；宿主可分帧用PRT3回复，捕获失败才调用
/// [`prime_fail_capture`]。窗口移动不清空场景，超出联合容量返回错误。
///
/// # Safety
/// camera 借出完整 WorldCamera；requests 借出 capacity*24 字节可写内存，二者不重叠。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_plan(
    id: u64,
    camera: *const WorldCamera,
    view_distance: u32,
    min_section_y: i32,
    max_section_y_exclusive: i32,
    requests: *mut u8,
    capacity: u32,
) -> i32 {
    guarded(-1, || {
        if camera.is_null() || requests.is_null() || capacity == 0 || capacity > 64 {
            return Err(Error::Input("捕获请求指针或容量无效"));
        }
        // SAFETY: 调用者借出完整相机，不要求对齐。
        let camera = unsafe { camera.read_unaligned() };
        let mut count = 0;
        with_live_engine(id, |engine| {
            engine.grow_view_budget(view_distance)?;
            let planned: &[scene::CaptureRequest] = engine.scene.plan(
                &camera,
                view_distance,
                min_section_y,
                max_section_y_exclusive,
                capacity as usize,
            )?;
            for (index, request) in planned.iter().enumerate() {
                let mut bytes = [0u8; 24];
                bytes[..8].copy_from_slice(&request.id.to_le_bytes());
                for axis in 0..3 {
                    bytes[8 + axis * 4..12 + axis * 4]
                        .copy_from_slice(&request.section[axis].to_le_bytes());
                }
                // SAFETY: plan 返回条数不超过 capacity；调用者提供完整可写输出。
                unsafe {
                    requests
                        .add(index * 24)
                        .copy_from_nonoverlapping(bytes.as_ptr(), 24)
                };
            }
            count = planned.len() as i32;
            engine.retire_tint_sources();
            let meshing = std::time::Instant::now();
            let result = engine.scene.compile_sources(&engine.catalog);
            engine.profile.plan_mesh_ns = engine
                .profile
                .plan_mesh_ns
                .saturating_add(elapsed_ns(meshing));
            result?;
            Ok(())
        })?;
        Ok(count)
    })
}

/// 失效一个已保留 section 的捕获源，旧可见资源保留到替换成功；未知 section 忽略。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_section(id: u64, sx: i32, sy: i32, sz: i32) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.tint_pending = None;
            Ok(engine.scene.invalidate_section([sx, sy, sz])?)
        })
    })
}

/// 同步消费PDI1变化批；完整验证后才入场景队列，几何与biome/tint依赖分别失效。
/// # Safety
/// data在调用内借出len字节只读连续内存；调用者独占会话，失败可保留同一包重试。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_invalidate_batch(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("变化包长度越界"))?;
        if data.is_null() || !(16..=scene::MAX_PACKET).contains(&len) {
            return Err(Error::Input("变化包指针或长度无效"));
        }
        // SAFETY: 调用者保持完整借用，上方已校验长度。
        let input = unsafe { std::slice::from_raw_parts(data, len) };
        let changes = changes::decode(input)?;
        with_live_engine(id, |engine| {
            engine.scene.apply_changes(&changes)?;
            engine.tint_pending = None;
            for change in &changes {
                match *change {
                    scene::SourceChange::BiomeColumn { x, z }
                    | scene::SourceChange::ColumnSource {
                        x, z, biome: true, ..
                    } => {
                        engine.tint_columns.insert((x, z));
                    }
                    scene::SourceChange::WorldTint => {
                        if let Some(world) = &mut engine.tint_world {
                            world.invalidate_colors();
                        }
                    }
                    scene::SourceChange::SourceOverflow => {
                        engine.tint_invalidate_all = true;
                        engine.tint_columns.clear();
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    })
}

/// 将全部已知源排入分步刷新；不清空可见场景或 RR 历史。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_all(id: u64) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.scene.invalidate_all();
            engine.tint_pending = None;
            engine.tint_invalidate_all = true;
            Ok(())
        })
    })
}

/// 标记一次宿主捕获失败；过期请求忽略，仍需要的 section 可在之后重试。
#[unsafe(no_mangle)]
pub extern "C" fn prime_fail_capture(id: u64, request: u64) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| Ok(engine.scene.fail_capture(request)?))
    })
}

/// 记录完整宿主帧间隔并导出PFT2主线程卡顿快照；成功返回写入字节数，失败返回-1。
///
/// elapsed_ns=0清空滚动窗口并跳过暂停间隔，保留会话最慢记录。
/// stages为14个little-endian u64纳秒：等待、tint/源维护、规划、宿主捕获、网格编译/合并、
/// 三角编码、属性打包、BLAS准备/等待、属性分配/写入、TLAS准备/等待、场景提交/回收、
/// native录制/SDK、宿主提交、未归因。最近场景更新规模按独立序号导出，非所选柱的规模。
/// plan内的同步编译从规划移到网格项；native场景细分从录制总耗时扣除，
/// 视图准备归场景准备/提交，余量由引擎统计补齐；不提供GPU timestamp。
/// columns为1..=512个实际图表像素列，输出容量至少728+120*columns且不超过62168。
/// columns=0时只记录，output须为空且capacity=0；隐藏图表不生成或传输窗口快照。
///
/// # Safety
/// stages借出至少112B只读内存，output借出capacity字节可写内存；二者不重叠，
/// 调用期间不得并发修改。该CPU接口不改变宿主GPU完成约定。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_profile(
    id: u64,
    elapsed_ns: u64,
    stages: *const u8,
    output: *mut u8,
    capacity: u32,
    columns: u32,
) -> i32 {
    guarded(-1, || {
        let columns = columns as usize;
        let capacity = capacity as usize;
        if stages.is_null()
            || columns > diagnostics::COLUMNS
            || (columns == 0 && (!output.is_null() || capacity != 0))
            || (columns != 0
                && (output.is_null()
                    || capacity < diagnostics::HEADER + columns * diagnostics::BAR
                    || capacity > diagnostics::HEADER + diagnostics::COLUMNS * diagnostics::BAR))
        {
            return Err(Error::Input("PFT2指针、容量或柱数无效"));
        }
        let timings = std::array::from_fn(|i| {
            // SAFETY: 调用者借出完整112B；不要求对齐。
            u64::from_le(unsafe { stages.add(i * 8).cast::<u64>().read_unaligned() })
        });
        let mut size = 0;
        with_engine(id, |engine| {
            engine.profile.set_pressure(engine.scene.pressure());
            engine.profile.set_work(
                engine.scene.work_stats(),
                engine.scene.work_budget().mesh_slabs,
            );
            engine.profile.record(elapsed_ns, timings)?;
            if columns != 0 {
                // SAFETY: 调用者独占完整可写输出；非空指针与长度上限均已检查。
                let output = unsafe { std::slice::from_raw_parts_mut(output, capacity) };
                size = engine.profile.write(output, columns)? as i32;
            }
            Ok(())
        })?;
        Ok(size)
    })
}

/// 同步消费PRT2捕获包；验证失败保留已发布场景，阶段资源按region ticket保留。
///
/// 所有字为 little-endian，坐标为 i32，其余整数为 u32，浮点为 f32：
/// `magic=0x32545250, section_count`；每段为 `request_id:u64,sx,sy,sz,block_count`；
/// 每块为 `x | y<<4 | z<<8, quad_count, offset_x,offset_y,offset_z`；
/// 每 quad 为 `ARGB, [x,y,z,u,v] × 4`。
/// quad 坐标相对方块，UV 对应 atlas；每包 1..=64 段，可跨 region。
/// PRT3保留原始palette/storage，收到必要邻域后由Rust编译；tint通过独立批阶段补齐。
/// 兼容PRT2中block_count=0表示真实空section；u32::MAX表示源chunk未加载。
/// 不可用region保留显式负缓存和已有GPU覆盖，加载通知后重新捕获完整region。
/// 旧ticket回复忽略，未知邻域不猜为空，失败不发布半个region。
///
/// # Safety
/// data 指向 len 字节的只读连续内存，调用期间保持活动且不被其他线程修改。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_batch(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("源包长度越界"))?;
        if data.is_null() || len == 0 || len > scene::MAX_PACKET {
            return Err(Error::Input("源包指针或长度无效"));
        }
        // SAFETY: 调用者保证整段内存只读且在调用内活动；上方限制长度。
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        with_live_engine(id, |engine| {
            if bytes.starts_with(&0x33545250u32.to_le_bytes()) {
                engine.scene.apply_compact(bytes, &engine.catalog)?;
                engine.retire_tint_sources();
            } else {
                engine.scene.apply(bytes)?;
            }
            Ok(())
        })
    })
}

/// 暂存下一代RGBA8 atlas；不改变当前可见GPU域，后续prime_sources统一准备并发布。
///
/// # Safety
/// pixels 指向 len 字节的只读连续内存。宿主已提交并确认前次 draw 完成，
/// 当前线程独占本会话；此调用只复制借用像素，GPU提交发生在prime_sources。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_atlas(
    id: u64,
    pixels: *const u8,
    len: u64,
    width: u32,
    height: u32,
) -> i32 {
    guarded(-1, || {
        let expected = u64::from(width) * u64::from(height) * 4;
        if pixels.is_null()
            || width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || expected != len
            || len > scene::MAX_PACKET as u64
        {
            return Err(Error::Input(
                "atlas 需要非空 RGBA8，长度匹配且不超过 256 MiB",
            ));
        }
        // SAFETY: 调用者保证在调用内借出的完整 atlas 内存；长度已验证。
        let bytes = unsafe { std::slice::from_raw_parts(pixels, len as usize) };
        with_live_engine(id, |engine| {
            let mut pixels = Vec::new();
            pixels
                .try_reserve_exact(bytes.len())
                .map_err(|_| Error::Input("待准备albedo容量不足"))?;
            pixels.extend_from_slice(bytes);
            engine.pending_albedo = Some(([width, height], pixels));
            Ok(())
        })
    })
}

/// 首次加载/重载的完整PRM1资源事务；失败保留旧Catalog、世界和GPU资源域。
/// 候选shader/SBT/材质/alias全部就绪后才发布，运行期section不得调用本入口追加资源。
///
/// # Safety
/// data在调用内借出完整len字节；宿主已完成此前命令且独占graphics queue。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_sources(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("资源包长度越界"))?;
        if data.is_null() || len == 0 || len > prime_minecraft::MAX_PACKET {
            return Err(Error::Input("资源包指针或长度无效"));
        }
        // SAFETY: 调用者保持源包只读活动，上方校验了完整长度。
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        with_live_engine(id, |engine| {
            let (extent, albedo) = engine
                .pending_albedo
                .as_ref()
                .ok_or(Error::Input("源资源准备前需要stage albedo"))?;
            let mut prepared = engine.catalog.stage_resources(bytes)?;
            let generation = prepared.generation();
            let world = engine.scene.stage_source_generation(generation)?;
            {
                let materials = prepared.material_resources(*extent, albedo)?;
                engine.gpu.replace_resources(gpu::ResourceUpload {
                    generation,
                    extent: *extent,
                    albedo,
                    materials: materials.materials,
                    mers: &materials.mers,
                    auxiliary: &materials.auxiliary,
                    emission_alias_words: &materials.emission_alias_words,
                    light_areas: &materials.quad_areas,
                })?;
            }
            prepared.commit();
            world.commit();
            engine.tint_world = None;
            engine.tint_pending = None;
            engine.tint_radius = None;
            engine.tint_columns.clear();
            engine.tint_invalidate_all = false;
            engine.pending_albedo = None;
            engine.atlas_ready = true;
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

/// 纯tint/biome缓存失效，不丢弃palette/storage或重新准备全局模型资源。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_tints(id: u64) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.scene.invalidate_tints();
            engine.tint_pending = None;
            if let Some(world) = &mut engine.tint_world {
                world.invalidate_colors();
            }
            Ok(())
        })
    })
}

/// 接受当前世界完整PBW1原始biome调色板与seed；失败保留旧世界tint所有者。
/// # Safety
/// data借出len个只读字节到返回，调用者独占本会话。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_biome_world(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("biome包长度越界"))?;
        if data.is_null() || len == 0 || len > prime_minecraft::MAX_PACKET {
            return Err(Error::Input("biome包指针或长度无效"));
        }
        // SAFETY: 长度已校验，调用者保持完整只读借用。
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        with_live_engine(id, |engine| {
            let resources = engine
                .catalog
                .tints()
                .ok_or(Error::Input("世界biome源前需要tint资源"))?;
            let world = prime_minecraft::tint::TintWorld::decode(bytes, resources)?;
            engine.tint_world = Some(world);
            engine.tint_pending = None;
            engine.tint_radius = None;
            engine.tint_columns.clear();
            engine.tint_invalidate_all = false;
            Ok(())
        })
    })
}

/// 完成纯Rust原版tint计划；缺少的quart与自定义回调由两个独立批阶段补齐。
#[unsafe(no_mangle)]
pub extern "C" fn prime_tint_begin(id: u64, radius: u32) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            if radius > 7 {
                return Err(Error::Input("tint半径须0..=7"));
            }
            engine.tint_pending = None;
            if engine.tint_radius.is_some_and(|old| old != radius) {
                engine.scene.invalidate_tints();
            }
            engine.tint_radius = Some(radius);
            if let Some(world) = engine.tint_world.as_mut()
                && (engine.tint_invalidate_all || !engine.tint_columns.is_empty())
            {
                world.invalidate(engine.tint_invalidate_all, &engine.tint_columns);
                engine.tint_columns.clear();
                engine.tint_invalidate_all = false;
            }
            let requests: Vec<_> = engine.scene.tint_requests().collect();
            if requests.is_empty() {
                return Ok(());
            }
            let resources = engine
                .catalog
                .tints()
                .ok_or(Error::Input("缺少全量tint recipe"))?;
            let world = engine
                .tint_world
                .as_mut()
                .ok_or(Error::Input("biome世界源尚未准备"))?;
            let plan = world.plan(&requests, resources, radius)?;
            let ids = plan.biome_requests().is_empty().then(Vec::new);
            let host = plan.host_requests().is_empty().then(Vec::new);
            engine.tint_pending = Some(TintContinuation { plan, ids, host });
            engine.finish_tints()
        })
    })
}

/// 复制原始quart请求，每条24B为sectionXYZ/reserved0/mask；capacity0查询页数。
/// # Safety
/// 非零capacity时output借出至少capacity*24个可写字节到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_biome_requests(id: u64, output: *mut u8, capacity: u32) -> i32 {
    guarded(-1, || {
        let mut count = 0;
        with_engine(id, |engine| {
            let Some(pending) = &engine.tint_pending else {
                return Ok(());
            };
            let requests = pending.plan.biome_requests();
            count = i32::try_from(requests.len()).map_err(|_| Error::Input("biome请求数量越界"))?;
            if capacity == 0 {
                return Ok(());
            }
            if output.is_null() || (capacity as usize) < requests.len() {
                return Err(Error::Input("biome请求输出容量不足"));
            }
            for (index, request) in requests.iter().enumerate() {
                let mut bytes = [0u8; 24];
                for axis in 0..3 {
                    bytes[axis * 4..axis * 4 + 4]
                        .copy_from_slice(&request.section[axis].to_le_bytes());
                }
                bytes[16..24].copy_from_slice(&request.mask.to_le_bytes());
                // SAFETY: 完整24B输出范围已校验，所有写入相互独立。
                unsafe {
                    output
                        .add(index * 24)
                        .copy_from_nonoverlapping(bytes.as_ptr(), 24);
                }
            }
            Ok(())
        })?;
        Ok(count)
    })
}

/// 完整回填quart请求的原始biome行号，按各mask升位顺序；不由Java求最终颜色。
/// # Safety
/// ids借出count个小端u32；允许未对齐，到返回前保持只读活动。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_biome_samples(id: u64, ids: *const u32, count: u32) -> i32 {
    guarded(-1, || {
        if ids.is_null() && count != 0 {
            return Err(Error::Input("biome响应指针为空"));
        }
        with_live_engine(id, |engine| {
            let pending = engine
                .tint_pending
                .as_mut()
                .ok_or(Error::Input("没有待续接biome计划"))?;
            if pending.plan.biome_sample_count() != count as usize || pending.ids.is_some() {
                return Err(Error::Input("biome响应数量不匹配或重复"));
            }
            let mut values = Vec::new();
            values
                .try_reserve_exact(count as usize)
                .map_err(|_| Error::Input("biome响应容量不足"))?;
            for index in 0..count as usize {
                // SAFETY: 调用者借出完整u32数组，上方校验数量；指针可未对齐。
                values.push(u32::from_le(unsafe { ids.add(index).read_unaligned() }));
            }
            pending.ids = Some(values);
            engine.finish_tints()
        })
    })
}

/// 复制唯一必要tint请求，每条24B为positionXYZ/state/slot/reserved0。
/// capacity=0只查询数量；在回填前宿主不得插入新源/资源世代修改。
///
/// # Safety
/// capacity非零时output指向至少capacity*24个可写字节；借用只持续本调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_tint_requests(id: u64, output: *mut u8, capacity: u32) -> i32 {
    guarded(-1, || {
        let mut result = 0;
        with_engine(id, |engine| {
            let requests = engine
                .tint_pending
                .as_ref()
                .map_or(&[][..], |pending| pending.plan.host_requests());
            result = i32::try_from(requests.len()).map_err(|_| Error::Input("tint请求数量越界"))?;
            if capacity == 0 {
                return Ok(());
            }
            if output.is_null() || (capacity as usize) < requests.len() {
                return Err(Error::Input("tint输出指针或容量无效"));
            }
            for (index, request) in requests.iter().enumerate() {
                let mut bytes = [0u8; 24];
                for axis in 0..3 {
                    bytes[axis * 4..axis * 4 + 4]
                        .copy_from_slice(&request.position[axis].to_le_bytes());
                }
                bytes[12..16].copy_from_slice(&request.state.to_le_bytes());
                bytes[16..20].copy_from_slice(&request.slot.to_le_bytes());
                // SAFETY: 数量和完整24B输出容量已校验，源输出不重叠。
                unsafe {
                    output
                        .add(index * 24)
                        .copy_from_nonoverlapping(bytes.as_ptr(), 24);
                }
            }
            Ok(())
        })?;
        Ok(result)
    })
}

/// 完整回填上次同步请求的真实ARGB，不接受数量不足的前缀或隐式白色。
///
/// # Safety
/// colors借出count个小端u32，允许未对齐；调用内不被修改，不夹入其他world变更。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_tints(id: u64, colors: *const u32, count: u32) -> i32 {
    guarded(-1, || {
        if colors.is_null() && count != 0 {
            return Err(Error::Input("tint颜色指针为空"));
        }
        with_live_engine(id, |engine| {
            let pending = engine
                .tint_pending
                .as_mut()
                .ok_or(Error::Input("没有待续接host tint计划"))?;
            if pending.plan.host_requests().len() != count as usize || pending.host.is_some() {
                return Err(Error::Input("tint响应数量不匹配或重复"));
            }
            let mut values = Vec::new();
            values
                .try_reserve_exact(count as usize)
                .map_err(|_| Error::Input("tint响应容量不足"))?;
            for index in 0..count as usize {
                // SAFETY: 调用者借出全部u32；不要求指针对齐。
                values.push(u32::from_le(unsafe { colors.add(index).read_unaligned() }));
            }
            pending.host = Some(values);
            engine.finish_tints()
        })
    })
}

/// 合批脏场景、构建加速结构并录制引擎 PT 与宿主呈现命令；不提交宿主 command。
///
/// # Safety
/// command 正在录制且不在 render pass 内；image/view 为本设备的 RGBA8_UNORM
/// STORAGE 图像，尺寸匹配，入口/出口布局均为 GENERAL（RenderPearl 26.3 约定）。
/// 输出第 0 行对应宿主画面底部；相机 up 保持世界向上的视锥向量。
/// camera 指向一份活动的 72B [`WorldCamera`]；函数返回后借用结束。
/// 场景和相机统一在 64m 网格 anchor 下计算；重定位保留 BLAS 与 RR 世界空间历史。
/// environment 指向一份活动的 16B [`FrameEnvironment`]。曝光只影响呈现。
/// rr=0 使用原始累积，rr=1 使用已完成早期 SDK 接入的 RR；其余值拒绝。
/// quality=0..=4 对应 DLAA/Quality/Balanced/Performance/UltraPerformance；SDK 决定输入尺寸。
/// samples=1..=64；RR 重建本帧样本均值，原始 PT 保留逐帧累积。bounces=1..=64 为最多表面反弹次数，默认12；末顶点保留发光与NEE。
/// [`prime_set_rendering`] 选择离线时固定最近已接受帧的场景、相机、光照和路径预算，
/// 只接受当前曝光、尺寸和每帧采样量；没有已接受帧时先绘制并接受首帧。离线禁用 RR，
/// 实时 ReSTIR 固定每帧一个初始候选，其余实时模式使用 samples。
/// 离线累积将超过2^24个样本时显式返回错误；由用户重新进入离线模式，不自动丢弃均值。
/// 累积失效不回退独立的绝对随机基数；完整帧在宿主完成边界接受，取消不发布新序号。
/// 宿主已确认前次 draw 完成，并独占 graphics queue；mesh 更新会同步提交构建命令。
/// 返回成功后宿主负责提交该 command，并在任何资源替换/销毁前等待完成。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_draw(
    id: u64,
    command: u64,
    image: u64,
    image_view: u64,
    width: u32,
    height: u32,
    camera: *const WorldCamera,
    environment: *const FrameEnvironment,
    rr: u32,
    quality: u32,
    samples: u32,
    bounces: u32,
) -> i32 {
    guarded(-1, || {
        if rr > 1
            || [command, image, image_view].contains(&0)
            || width == 0
            || height == 0
            || camera.is_null()
            || environment.is_null()
        {
            return Err(Error::Input("draw 句柄、尺寸、camera 或 environment 无效"));
        }
        // SAFETY: 调用者借出至少一份 WorldCamera；不要求源指针对齐。
        let source = unsafe { camera.read_unaligned() };
        let source_anchor = source.anchor()?;
        source.relative(source_anchor)?;
        // SAFETY: 调用者借出完整帧环境包，不要求源指针对齐。
        let source_environment = unsafe { environment.read_unaligned() };
        let options = gpu::Options::new(
            rr != 0,
            streamline::Quality::try_from(quality)?,
            samples,
            bounces,
        )?;
        with_engine(id, |engine| {
            let incoming = source_environment.resolve(engine.solar_orbit, source_anchor[1])?;
            let frozen = engine.rendering.frozen();
            let frame = engine.rendering.choose(rendering::Frame {
                camera: source,
                environment: incoming,
                bounces,
            });
            let anchor = frame.camera.anchor()?;
            let camera = frame.camera.relative(anchor)?;
            let environment = frame.environment;
            let options = if engine.rendering.mode() == prime_scene::RenderMode::Offline {
                gpu::Options::new(
                    false,
                    streamline::Quality::try_from(quality)?,
                    samples,
                    frame.bounces,
                )?
            } else {
                options
            };
            if !engine.atlas_ready {
                return Err(Error::Input("draw 前需要上传 atlas"));
            }
            engine.profile.scene = [0; 7];
            engine.profile.update = None;
            let prepare_started = std::time::Instant::now();
            if !frozen {
                engine.scene.rebase(anchor);
            }
            let changed =
                !frozen && (engine.scene.prepare()? || engine.gpu.scene_resources_dirty());
            engine.profile.scene[0] = elapsed_ns(prepare_started);
            if changed {
                let scene_started = std::time::Instant::now();
                let publication = (|| {
                    let changes: Vec<_> = engine.scene.render_changes().collect();
                    if engine.gpu.try_attribute_changes(
                        engine.scene.generation(),
                        engine.scene.anchor(),
                        &changes,
                    )? {
                        return Ok(());
                    }
                    let regions: Vec<_> = engine.scene.render_regions().collect();
                    engine.gpu.set_regions(prime_scene::RenderView {
                        generation: engine.scene.generation(),
                        regions: &regions,
                        anchor: engine.scene.anchor(),
                    })
                })();
                if let Err(error) = publication {
                    engine.scene.reject()?;
                    return Err(error.into());
                }
                engine.scene.commit()?;
                let timing = engine.gpu.scene_timing().expect("成功发布保存当前计时");
                engine.profile.scene[1] = timing
                    .geometry_ns
                    .saturating_sub(timing.attribute_pack_ns + timing.light_pack_ns);
                engine.profile.scene[2] = timing.attribute_pack_ns + timing.light_pack_ns;
                engine.profile.scene[3] = timing.acceleration_ns.saturating_sub(timing.tlas_ns);
                engine.profile.scene[4] = timing.attribute_upload_ns + timing.light_upload_ns;
                engine.profile.scene[5] = timing.tlas_ns;
                engine.profile.scene[6] = elapsed_ns(scene_started).saturating_sub(
                    timing
                        .geometry_ns
                        .saturating_add(timing.acceleration_ns)
                        .saturating_add(timing.attribute_upload_ns + timing.light_upload_ns),
                );
                engine.profile.update = Some(diagnostics::SceneInfo {
                    dirty_regions: timing.dirty_regions as u64,
                    instances: timing.instances as u64,
                    triangle_bytes: timing.triangle_bytes,
                    attribute_bytes: timing.attribute_upload_bytes
                        + timing.attribute_copy_bytes
                        + timing.light_upload_bytes,
                    as_wait_ns: timing.as_submit_wait_ns,
                    tlas_ns: timing.tlas_ns,
                    attribute_ns: timing.attribute_upload_ns + timing.light_upload_ns,
                });
            }
            if engine.rendering.mode() == prime_scene::RenderMode::Offline
                && engine.gpu.accumulated_samples() > (1 << 24) - samples
            {
                return Err(Error::Input(
                    "离线样本达到精确累积上限；请显式重新进入离线模式",
                ));
            }
            engine.gpu.draw(
                command,
                image,
                image_view,
                [width, height],
                &camera,
                environment,
                options,
            )?;
            engine.rendering.accepted(frame);
            Ok(())
        })
    })
}

fn elapsed_ns(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
