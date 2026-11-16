//! 宿主设备与资源根的创建、回收及错误边界；C ABI保持不变。

use super::*;

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

/// 带设备能力声明的会话创建；flags bit0 声明 EXT SER，bit1 关闭初始 SER，bit2 启用 NV 异步检查点，bit3 声明 KHR OMM，其余位拒绝。
/// bit1 只允许与 bit0 同时设置；失败返回 0。
///
/// # Safety
/// 与 [`prime_create`] 相同；bit0 为 1 时设备还须真正启用
/// `VK_EXT_ray_tracing_invocation_reorder` 与 `rayTracingInvocationReorder`。
/// 初始选择由 bit1 决定，之后可用 [`prime_set_ser`] 切换；不以物理支持替代逻辑设备启用。
/// bit2 为 1 时必须在该逻辑设备启用 `VK_NV_device_diagnostic_checkpoints`。
/// bit3 为 1 时必须启用 `VK_KHR_opacity_micromap` 及其 `micromap` 特性。
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
        if flags & !15 != 0 || flags & 3 == 2 {
            return Err(Error::Input("未知设备能力 flags"));
        }
        if [instance, physical, device, queue].contains(&0) {
            return Err(Error::Input("Vulkan 句柄不能为空"));
        }
        let capture = capture::Capture::from_env()?;
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
                flags & 8 != 0,
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
                    capture,
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
