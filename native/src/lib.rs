//! Minecraft 26.3 的引擎 PT 场景适配实验。
//!
//! Java 在渲染线程同步借出源包、atlas 与宿主 Vulkan 句柄；Rust 拥有 mesh、
//! 工作池、BLAS/TLAS、SBT 和 pipeline。每次进入 native 前，宿主必须提交并等待
//! 前次 native 命令完成，且独占 graphics queue。句柄不会转移设备所有权。
//! 源包只在调用内借用；解码、验证和 CPU mesh 替换在返回前完成。
//! 使用 [`Quad`] 的直接寻址属性 ABI：任意四角、固定012/023、三角面退化为quad，
//! 不要求轴对齐。位置只由 AS position fetch 提供，不保留 shader 顶点/索引副本。
//! 宿主必须启用 [KHR position fetch] 扩展和 feature，无回退；能力边界见 [`Quad`]。
//!
//! [KHR position fetch]: https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_ray_tracing_position_fetch.html

#![recursion_limit = "256"]

mod gpu;
mod reconstruction;
mod scene;
mod streamline;

pub use scene::{Camera, Environment, Material, Mesh, Quad, WorldCamera};

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
    /// 无法创建批次工作池。
    #[error("mesh 工作池：{0}")]
    Workers(#[from] rayon::ThreadPoolBuildError),
    /// Vulkan 加载、资源或命令失败。
    #[error(transparent)]
    Gpu(#[from] gpu::Error),
    /// Streamline 初始化或全局退役失败。
    #[error(transparent)]
    Streamline(#[from] streamline::Error),
    /// 区块窗口配置不符合引擎约定。
    #[error(transparent)]
    Config(#[from] chunk_manager::ConfigError),
    /// 区块资源状态转换失败。
    #[error(transparent)]
    Chunks(#[from] chunk_manager::StateError),
    /// 区块调度输入无效。
    #[error(transparent)]
    Planning(#[from] chunk_manager::PlanError),
}

struct Engine {
    scene: scene::Scene,
    gpu: gpu::Gpu,
    atlas_ready: bool,
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

/// 返回当前线程最后一次失败信息；UTF-8 指针有效至该线程下一次失败。
#[unsafe(no_mangle)]
pub extern "C" fn prime_error() -> *const c_char {
    LAST_ERROR.with(|slot| slot.borrow().as_ptr())
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
/// 设备须启用 deviceAddressCommands、synchronization2、bufferDeviceAddress、accelerationStructure、rayTracingPipeline、rayQuery、
/// rayTracingPositionFetch 及其扩展；BLAS 必须使用 ALLOW_DATA_ACCESS，无顶点副本回退。
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

/// 带设备能力声明的会话创建；flags bit0 声明 EXT SER，bit1 关闭初始 SER，其余位拒绝。
/// bit1 只允许与 bit0 同时设置；失败返回 0。
///
/// # Safety
/// 与 [`prime_create`] 相同；bit0 为 1 时设备还须真正启用
/// `VK_EXT_ray_tracing_invocation_reorder` 与 `rayTracingInvocationReorder`。
/// 初始选择由 bit1 决定，之后可用 [`prime_set_ser`] 切换；不以物理支持替代逻辑设备启用。
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
        if flags & !3 != 0 || flags == 2 {
            return Err(Error::Input("未知设备能力 flags"));
        }
        if [instance, physical, device, queue].contains(&0) {
            return Err(Error::Input("Vulkan 句柄不能为空"));
        }
        let scene = scene::Scene::new()?;
        // SAFETY: 调用者提供同一活动设备并独占 queue，见导出接口契约。
        let gpu = unsafe {
            gpu::Gpu::attach_options(
                instance,
                physical,
                device,
                queue,
                family,
                queue_index,
                flags & 1 != 0,
                flags & 2 == 0,
            )?
        };
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
                    gpu,
                    atlas_ready: false,
                },
            );
        Ok(id)
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
        let engine = ENGINES
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| Error::Input("native 会话锁已损坏"))?
            .remove(&id)
            .ok_or(Error::Input("无效 native 句柄"))?;
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
            engine.scene.clear();
            engine
                .gpu
                .invalidate_history(gpu::GlobalReset::WorldReplaced)?;
            Ok(())
        })
    })
}

/// 为当前相机推进引擎 LOD0 窗口，返回至多 capacity 条捕获请求；失败返回 -1。
///
/// 请求为 little-endian 的 `id:u64, sx:i32, sy:i32, sz:i32, reserved:u32=0`，每条 24B。
/// 宿主须用 PRT2 回复，或对未完成请求调用 [`prime_fail_capture`]；窗口移动不清空场景。
///
/// # Safety
/// camera 借出完整 WorldCamera；requests 借出 capacity*24 字节可写内存，二者不重叠。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_plan(
    id: u64,
    camera: *const WorldCamera,
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
        with_engine(id, |engine| {
            let planned: &[scene::CaptureRequest] =
                engine.scene.plan(&camera, capacity as usize)?;
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
            Ok(())
        })?;
        Ok(count)
    })
}

/// 失效一个已保留 section 的捕获源，旧可见资源保留到替换成功；未知 section 忽略。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_section(id: u64, sx: i32, sy: i32, sz: i32) -> i32 {
    guarded(-1, || {
        with_engine(id, |engine| engine.scene.invalidate_section([sx, sy, sz]))
    })
}

/// 将全部已知源排入分步刷新；不清空可见场景或 RR 历史。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_all(id: u64) -> i32 {
    guarded(-1, || {
        with_engine(id, |engine| {
            engine.scene.invalidate_all();
            Ok(())
        })
    })
}

/// 标记一次宿主捕获失败；过期请求忽略，仍需要的 section 可在之后重试。
#[unsafe(no_mangle)]
pub extern "C" fn prime_fail_capture(id: u64, request: u64) -> i32 {
    guarded(-1, || {
        with_engine(id, |engine| engine.scene.fail_capture(request))
    })
}

/// 同步消费一个 PRT2 源包；验证失败不修改 CPU 场景，成功返回 0。
///
/// 所有字为 little-endian，坐标为 i32，其余整数为 u32，浮点为 f32：
/// `magic=0x32545250, section_count`；每段为 `request_id:u64,sx,sy,sz,block_count`；
/// 每块为 `x | y<<4 | z<<8, quad_count, offset_x,offset_y,offset_z`；
/// 每 quad 为 `ARGB, [x,y,z,u,v] × 4`。
/// quad 坐标相对方块，UV 对应 atlas；每包 1..=64 段，可跨 region。
/// 空段仍是已完成的 LOD0 空资源；旧 ticket 回复忽略，原版面剔除在捕获边界求值。
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
        with_engine(id, |engine| engine.scene.apply(bytes))
    })
}

/// 上传一代 RGBA8 atlas；UV 使用整张图归一化坐标，成功返回 0。
///
/// # Safety
/// pixels 指向 len 字节的只读连续内存。宿主已提交并确认前次 draw 完成，
/// 当前线程独占 graphics queue；此调用可能同步提交上传命令。
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
        with_engine(id, |engine| {
            engine.gpu.set_atlas(width, height, bytes)?;
            engine
                .gpu
                .invalidate_history(gpu::GlobalReset::SceneDomainReplaced)?;
            engine.scene.clear();
            engine.atlas_ready = true;
            Ok(())
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
/// environment 指向一份活动的 [`Environment`]；曝光只影响呈现，零太阳禁用大气。
/// rr=0 使用原始累积，rr=1 使用已完成早期 SDK 接入的 RR；其余值拒绝。
/// quality=0..=4 对应 DLAA/Quality/Balanced/Performance/UltraPerformance；SDK 决定输入尺寸。
/// samples=1..=64；RR 重建本帧样本均值，原始 PT 保留逐帧累积。bounces=1..=64 为路径顶点预算。
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
    environment: *const Environment,
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
        let anchor = source.anchor()?;
        let camera = source.relative(anchor)?;
        // SAFETY: 调用者借出完整 Environment；不要求源指针对齐。
        let mut environment = unsafe { environment.read_unaligned() }.validated()?;
        environment.sea_level = (f64::from(environment.sea_level) - anchor[1]) as f32;
        let options = gpu::Options::new(
            rr != 0,
            streamline::Quality::try_from(quality)?,
            samples,
            bounces,
        )?;
        with_engine(id, |engine| {
            if !engine.atlas_ready {
                return Err(Error::Input("draw 前需要上传 atlas"));
            }
            engine.scene.rebase(anchor);
            if engine.scene.prepare()? {
                if let Err(error) = engine.gpu.set_sections(&engine.scene) {
                    engine.scene.reject()?;
                    return Err(error.into());
                }
                engine.scene.commit()?;
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
            Ok(())
        })
    })
}
