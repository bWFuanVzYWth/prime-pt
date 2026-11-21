//! 独立无窗口设备上的真实 trace/readback；显式运行 ignored 测试。

use super::*;
use crate::scene::{Material, Quad};
use ash::{
    Entry, vk,
    vk::{Handle, TaggedStructure},
};
use simple_engine::{shader_build, vulkan};
use std::ffi::CStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{cell::Cell, io::Write as _, mem::ManuallyDrop};

#[path = "scene_bench.rs"]
mod scene_bench;

#[path = "capture_tests.rs"]
mod capture_tests;

#[path = "features_tests.rs"]
mod features_tests;
#[path = "history_tests.rs"]
mod history_tests;
#[path = "region_update_tests.rs"]
mod region_update_tests;

#[path = "rendering_tests.rs"]
mod rendering_tests;

#[path = "terrain_volume_tests.rs"]
mod terrain_volume_tests;

#[path = "pt_quality_bench.rs"]
mod pt_quality_bench;

struct Host {
    _entry: ManuallyDrop<Entry>,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    errors: ManuallyDrop<Box<AtomicUsize>>,
    streamline: Option<StreamlineBootstrap>,
    invocation_reorder: bool,
    opacity_micromap: bool,
    diagnostic_checkpoints: bool,
    retained: Cell<bool>,
}

struct StreamlineBootstrap(bool);

impl StreamlineBootstrap {
    fn close(&mut self) -> Result<(), streamline::Error> {
        if self.0 {
            crate::streamline::shutdown()?;
            self.0 = false;
        }
        Ok(())
    }
}

impl Drop for StreamlineBootstrap {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            let _ = writeln!(std::io::stderr().lock(), "测试 SDK 退役失败：{error}");
        }
    }
}

/// 测试会话借用宿主，退出前先建立 GPU 完成事实；失败时保留整套依赖所有者。
struct Session<'a> {
    host: &'a Host,
    gpu: Option<Gpu>,
}

impl Session<'_> {
    fn gpu(&mut self) -> &mut Gpu {
        self.gpu.as_mut().expect("会话仅在消费退出或析构中移出 GPU")
    }

    fn finish(mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.release()
    }

    fn release(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(mut gpu) = self.gpu.take() else {
            return Ok(());
        };
        // SAFETY: 会话独占 GPU；宿主由借用保活，测试没有并发提交。
        let completion = unsafe { self.host.device.queue_wait_idle(self.host.queue) };
        if self.host.retained.get()
            || !matches!(completion, Ok(()) | Err(vk::Result::ERROR_DEVICE_LOST))
        {
            self.host.retained.set(true);
            std::mem::forget(gpu);
            return Err(completion.err().map_or_else(
                || "测试宿主因先前退役失败而保留".into(),
                |error| Box::new(error) as Box<dyn std::error::Error>,
            ));
        }
        if let Err(error) = gpu.close() {
            self.host.retained.set(true);
            std::mem::forget(gpu);
            return Err(error.into());
        }
        drop(gpu);
        completion?;
        Ok(())
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            self.host.errors.fetch_add(1, Ordering::Relaxed);
            let _ = writeln!(std::io::stderr().lock(), "测试会话退役失败：{error}");
        }
    }
}

unsafe extern "system" fn diagnostic(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut std::ffi::c_void,
) -> vk::Bool32 {
    unsafe {
        let message = CStr::from_ptr((*data).p_message).to_string_lossy();
        // AGENTS.md 中用户批准的第三方例外；原始消息仍完整写入日志。
        let body = message
            .split_once('\n')
            .filter(|(heading, _)| {
                heading.starts_with("(Warning - This VUID has now been reported ")
                    && heading.contains("duplicate_message_limit")
            })
            .map_or(message.as_ref(), |(_, detail)| detail);
        let known_sl = body.starts_with("vkCmdClearColorImage(): WRITE_AFTER_WRITE")
            && body.contains("[nv.ngx.dlssd.resource]")
            && body.contains("vkCmdPipelineBarrier[nv.ngx.dlssd.Evaluate]")
            && body.contains("VK_ACCESS_2_TRANSFER_WRITE_BIT")
            && body.contains("VK_PIPELINE_STAGE_2_CLEAR_BIT");
        if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) && !known_sl {
            (*user.cast::<AtomicUsize>()).fetch_add(1, Ordering::Relaxed);
        }
        if known_sl {
            let _ = writeln!(
                std::io::stderr().lock(),
                "known_third_party: SL 内部 clear 同步诊断，按 AGENTS.md 忽略 1 条"
            );
        }
        let _ = writeln!(std::io::stderr().lock(), "validation: {message}");
    }
    vk::FALSE
}

impl Host {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::create(false, false)
    }

    fn new_rr() -> Result<Self, Box<dyn std::error::Error>> {
        Self::create(true, false)
    }

    fn new_rr_ser() -> Result<Self, Box<dyn std::error::Error>> {
        Self::create(true, true)
    }

    fn new_ser() -> Result<Self, Box<dyn std::error::Error>> {
        Self::create(false, true)
    }

    fn create(rr: bool, invocation_reorder: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let streamline = if rr {
            let executable = std::env::current_exe()?;
            let directory = executable.parent().ok_or("测试程序目录缺失")?;
            let profile = directory
                .ancestors()
                .find(|path| {
                    path.file_name().is_some_and(|name| {
                        name == "debug" || name == "release" || name == "release-lto"
                    })
                })
                .ok_or("测试程序不在 Cargo profile 内")?;
            // 测试链接产物与 build-script 的 OUT_DIR hash 不同；只复制可再生 runtime。
            if profile != directory {
                for name in [
                    "sl.interposer.dll",
                    "sl.common.dll",
                    "sl.dlss_d.dll",
                    "nvngx_dlssd.dll",
                ] {
                    std::fs::copy(profile.join(name), directory.join(name))?;
                }
            }
            crate::streamline::bootstrap()?;
            Some(StreamlineBootstrap(true))
        } else {
            None
        };
        unsafe {
            let entry = if rr {
                Entry::load_from(
                    std::env::current_exe()?
                        .parent()
                        .ok_or("测试程序目录缺失")?
                        .join("sl.interposer.dll"),
                )?
            } else {
                Entry::load()?
            };
            if entry
                .try_enumerate_instance_version()?
                .unwrap_or(vk::API_VERSION_1_0)
                < vk::API_VERSION_1_4
            {
                return Err("测试需要 Vulkan 1.4 loader".into());
            }
            let validation = entry
                .enumerate_instance_layer_properties()?
                .iter()
                .any(|layer| {
                    CStr::from_ptr(layer.layer_name.as_ptr()) == c"VK_LAYER_KHRONOS_validation"
                });
            let layers = if validation {
                vec![c"VK_LAYER_KHRONOS_validation".as_ptr()]
            } else {
                Vec::new()
            };
            let extensions = if validation {
                vec![ash::ext::debug_utils::NAME.as_ptr()]
            } else {
                Vec::new()
            };
            let errors = Box::new(AtomicUsize::new(0));
            let mut debug = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(diagnostic))
                .user_data((&*errors as *const AtomicUsize).cast_mut().cast());
            let application = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_4);
            let mut info = vk::InstanceCreateInfo::default()
                .application_info(&application)
                .enabled_layer_names(&layers)
                .enabled_extension_names(&extensions);
            if validation {
                info = info.push(&mut debug);
            }
            let instance = entry.create_instance(&info, None)?;
            let debug = if validation {
                let extension = ash::ext::debug_utils::Instance::load(&entry, &instance);
                let messenger = extension.create_debug_utils_messenger(&debug, None)?;
                Some((extension, messenger))
            } else {
                eprintln!("VK_LAYER_KHRONOS_validation 未安装；仅执行硬件 readback");
                None
            };
            let mut host = Self {
                _entry: ManuallyDrop::new(entry),
                instance,
                physical: vk::PhysicalDevice::null(),
                device: ash::Device::load_with(|_| std::ptr::null(), vk::Device::null()),
                queue: vk::Queue::null(),
                family: 0,
                debug,
                errors: ManuallyDrop::new(errors),
                streamline,
                invocation_reorder,
                opacity_micromap: false,
                diagnostic_checkpoints: std::env::var_os("PRIME_PT_DIAGNOSTIC_CHECKPOINTS")
                    .is_some_and(|value| value == "1"),
                retained: Cell::new(false),
            };
            let mut required = vec![
                ash::khr::acceleration_structure::NAME,
                ash::khr::ray_tracing_pipeline::NAME,
                ash::khr::ray_query::NAME,
                ash::khr::deferred_host_operations::NAME,
                ash::khr::ray_tracing_position_fetch::NAME,
                ash::khr::device_address_commands::NAME,
            ];
            if rr {
                required.extend([
                    ash::nvx::binary_import::NAME,
                    ash::nvx::image_view_handle::NAME,
                    ash::khr::push_descriptor::NAME,
                    ash::khr::buffer_device_address::NAME,
                    ash::khr::synchronization2::NAME,
                ]);
            }
            if invocation_reorder {
                required.push(ash::ext::ray_tracing_invocation_reorder::NAME);
            }
            if host.diagnostic_checkpoints {
                required.push(ash::nv::device_diagnostic_checkpoints::NAME);
            }
            for physical in host.instance.enumerate_physical_devices()? {
                if host
                    .instance
                    .get_physical_device_properties(physical)
                    .api_version
                    < vk::API_VERSION_1_4
                {
                    continue;
                }
                let extensions = host
                    .instance
                    .enumerate_device_extension_properties(physical)?;
                if !required.iter().all(|name| {
                    extensions
                        .iter()
                        .any(|extension| CStr::from_ptr(extension.extension_name.as_ptr()) == *name)
                }) {
                    continue;
                }
                let Some(family) = host
                    .instance
                    .get_physical_device_queue_family_properties(physical)
                    .iter()
                    .position(|family| {
                        family.queue_flags.contains(if rr {
                            vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE
                        } else {
                            vk::QueueFlags::COMPUTE
                        })
                    })
                else {
                    continue;
                };
                let supports_omm = extensions.iter().any(|extension| {
                    CStr::from_ptr(extension.extension_name.as_ptr())
                        == ash::khr::opacity_micromap::NAME
                });
                let push_descriptors = extensions.iter().any(|e| {
                    CStr::from_ptr(e.extension_name.as_ptr()) == ash::khr::push_descriptor::NAME
                });
                let mut omm = vk::PhysicalDeviceOpacityMicromapFeaturesKHR::default();
                if supports_omm {
                    host.instance.get_physical_device_features2(
                        physical,
                        &mut vk::PhysicalDeviceFeatures2::default().push(&mut omm),
                    );
                }
                host.opacity_micromap = supports_omm && omm.micromap != 0;
                let mut commands = vk::PhysicalDeviceDeviceAddressCommandsFeaturesKHR::default();
                let mut acceleration =
                    vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
                let mut tracing = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::default();
                let mut query = vk::PhysicalDeviceRayQueryFeaturesKHR::default();
                let mut position = vk::PhysicalDeviceRayTracingPositionFetchFeaturesKHR::default();
                let mut core12 = vk::PhysicalDeviceVulkan12Features::default();
                let mut core13 = vk::PhysicalDeviceVulkan13Features::default();
                let mut reorder =
                    vk::PhysicalDeviceRayTracingInvocationReorderFeaturesEXT::default();
                let mut features = vk::PhysicalDeviceFeatures2::default()
                    .push(&mut commands)
                    .push(&mut acceleration)
                    .push(&mut tracing)
                    .push(&mut query)
                    .push(&mut position);
                features = features.push(&mut core12).push(&mut core13);
                if invocation_reorder {
                    features = features.push(&mut reorder);
                }
                host.instance
                    .get_physical_device_features2(physical, &mut features);
                let supported = features.features;
                let nrd = push_descriptors
                    && core12.descriptor_binding_partially_bound != 0
                    && supported.shader_storage_image_write_without_format != 0
                    && supported.shader_int16 != 0
                    && core12.shader_float16 != 0;
                if core12.buffer_device_address == 0
                    || core13.synchronization2 == 0
                    || core13.subgroup_size_control == 0
                    || commands.device_address_commands == 0
                    || acceleration.acceleration_structure == 0
                    || tracing.ray_tracing_pipeline == 0
                    || query.ray_query == 0
                    || position.ray_tracing_position_fetch == 0
                    || supported.shader_storage_image_extended_formats == 0
                    || (invocation_reorder && reorder.ray_tracing_invocation_reorder == 0)
                    || (rr
                        && (core12.timeline_semaphore == 0
                            || core12.descriptor_indexing == 0
                            || core13.synchronization2 == 0
                            || core13.private_data == 0
                            || supported.shader_storage_image_write_without_format == 0))
                {
                    continue;
                }
                let priorities = [1.0];
                let queues = [vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(family as u32)
                    .queue_priorities(&priorities)];
                let mut extensions: Vec<_> = required.iter().map(|name| name.as_ptr()).collect();
                if nrd {
                    extensions.push(ash::khr::push_descriptor::NAME.as_ptr());
                }
                if host.opacity_micromap {
                    extensions.push(ash::khr::opacity_micromap::NAME.as_ptr());
                }
                let mut commands = vk::PhysicalDeviceDeviceAddressCommandsFeaturesKHR::default()
                    .device_address_commands(true);
                let mut acceleration =
                    vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default()
                        .acceleration_structure(true);
                let mut tracing = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::default()
                    .ray_tracing_pipeline(true);
                let mut query = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);
                let mut position = vk::PhysicalDeviceRayTracingPositionFetchFeaturesKHR::default()
                    .ray_tracing_position_fetch(true);
                let mut core12 = vk::PhysicalDeviceVulkan12Features::default()
                    .descriptor_binding_partially_bound(nrd)
                    .shader_float16(nrd)
                    .buffer_device_address(true)
                    .timeline_semaphore(rr)
                    .descriptor_indexing(rr);
                let mut core13 = vk::PhysicalDeviceVulkan13Features::default()
                    .synchronization2(true)
                    .subgroup_size_control(true)
                    .private_data(rr);
                let mut reorder =
                    vk::PhysicalDeviceRayTracingInvocationReorderFeaturesEXT::default()
                        .ray_tracing_invocation_reorder(true);
                let enabled = vk::PhysicalDeviceFeatures::default()
                    .shader_storage_image_extended_formats(true)
                    .shader_int16(nrd)
                    .shader_storage_image_write_without_format(rr || nrd);
                let mut device_info = vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&extensions)
                    .enabled_features(&enabled)
                    .push(&mut commands)
                    .push(&mut acceleration)
                    .push(&mut tracing)
                    .push(&mut query)
                    .push(&mut position);
                device_info = device_info.push(&mut core12).push(&mut core13);
                if invocation_reorder {
                    device_info = device_info.push(&mut reorder);
                }
                if host.opacity_micromap {
                    device_info = device_info.push(&mut omm);
                }
                host.device = host.instance.create_device(physical, &device_info, None)?;
                host.physical = physical;
                host.family = family as u32;
                host.queue = host.device.get_device_queue(host.family, 0);
                let properties = host.instance.get_physical_device_properties(physical);
                eprintln!(
                    "硬件光追 smoke：{}，instance=1.4 physical={}.{} deviceAddressCommands=true synchronization2=true EXT_SER={invocation_reorder}",
                    CStr::from_ptr(properties.device_name.as_ptr()).to_string_lossy(),
                    vk::api_version_major(properties.api_version),
                    vk::api_version_minor(properties.api_version)
                );
                return Ok(host);
            }
            Err("没有支持硬件光追管线的 Vulkan 设备".into())
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            if self.retained.get() {
                std::mem::forget(self.streamline.take());
                return;
            }
            if self.device.handle() != vk::Device::null() {
                let completion = self.device.device_wait_idle();
                if !matches!(completion, Ok(()) | Err(vk::Result::ERROR_DEVICE_LOST)) {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "测试宿主保留在途设备：{completion:?}"
                    );
                    std::mem::forget(self.streamline.take());
                    return;
                }
            }
            if let Some(bootstrap) = &mut self.streamline
                && let Err(error) = bootstrap.close()
            {
                let _ = writeln!(std::io::stderr().lock(), "测试宿主保留 SDK 设备：{error}");
                std::mem::forget(self.streamline.take());
                return;
            }
            if self.device.handle() != vk::Device::null() {
                self.device.destroy_device(None);
            }
            if let Some((extension, messenger)) = &self.debug {
                extension.destroy_debug_utils_messenger(*messenger, None);
            }
            self.instance.destroy_instance(None);
            // SAFETY: SDK 已退出且所有 Vulkan 对象已销毁，回调和 loader 不再被引用。
            ManuallyDrop::drop(&mut self.errors);
            ManuallyDrop::drop(&mut self._entry);
        }
    }
}

fn add_quad(mesh: &mut Mesh, z: f32, uv: [f32; 2], tint: [f32; 3], half_size: f32) {
    let positions = [
        [-half_size, -half_size, z],
        [half_size, -half_size, z],
        [half_size, half_size, z],
        [-half_size, half_size, z],
    ];
    mesh.quads
        .push(Quad::new(positions, [uv; 4], tint, Material::Opaque));
}

impl Host {
    fn gpu(&self) -> Result<Session<'_>, Error> {
        self.gpu_requested(true)
    }

    fn gpu_requested(&self, requested: bool) -> Result<Session<'_>, Error> {
        if self.retained.get() {
            return Err(Error::Invalid("测试宿主已因退役失败而隔离"));
        }
        // SAFETY: Host 比会话活得更久；测试同步提交，销毁前没有在途命令。
        let mut gpu = unsafe {
            Gpu::attach_options(
                self.instance.handle().as_raw(),
                self.physical.as_raw(),
                self.device.handle().as_raw(),
                self.queue.as_raw(),
                self.family,
                0,
                self.invocation_reorder,
                self.opacity_micromap,
                requested,
                None,
            )?
        };
        if self.diagnostic_checkpoints {
            // SAFETY: Host::create 在该测试逻辑设备显式启用同一 NV 扩展。
            unsafe { gpu.enable_diagnostic_checkpoints()? };
        }
        gpu.settings.samples = pt::REALTIME_SAMPLES;
        gpu.settings.max_bounces = 6;
        gpu.display_timeline.set_enabled(false);
        Ok(Session {
            host: self,
            gpu: Some(gpu),
        })
    }

    fn render(
        &self,
        gpu: &mut Gpu,
        output: &Image,
        camera: &Camera,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.render_environment(gpu, output, camera, Environment::default())
    }

    fn render_environment(
        &self,
        gpu: &mut Gpu,
        output: &Image,
        camera: &Camera,
        environment: Environment,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let [width, height, _] = output.extent();
        // SAFETY: 测试独占本设备和队列，命令及 Flight 一直保留至 queue idle 成功。
        unsafe {
            let pool = self.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(self.family),
                None,
            )?;
            let mut records = Vec::new();
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                records = gpu.record(
                    [width, height],
                    camera,
                    environment,
                    Binding::StorageImage(*output),
                )?;
                let command = self.device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1)
                        .level(vk::CommandBufferLevel::PRIMARY),
                )?[0];
                self.device
                    .begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())?;
                gpu.encode_recordings(std::mem::take(&mut records), command.as_raw())?;
                self.device.end_command_buffer(command)?;
                self.device.queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&[command])],
                    vk::Fence::null(),
                )?;
                Ok(())
            })();
            if let Err(error) = self.device.queue_wait_idle(self.queue) {
                if error == vk::Result::ERROR_DEVICE_LOST {
                    self.device.destroy_command_pool(pool, None);
                } else {
                    self.retained.set(true);
                }
                return Err(error.into());
            }
            if result.is_err() {
                gpu.display_timeline.cancel();
                gpu.cancel_recordings(std::mem::take(&mut records))?;
            }
            gpu.complete_previous()?;
            self.device.destroy_command_pool(pool, None);
            if result.is_err() {
                gpu.cancel_recordings(records)?;
                gpu.transition_history(history::Event::Accumulation(
                    history::AccumulationEvent::RecordingCancelled,
                ))?;
            } else {
                gpu.atmosphere.submitted();
            }
            result?;
        }
        Ok(output.download(&mut gpu.context)?)
    }

    fn render_rr(
        &self,
        gpu: &mut Gpu,
        output: &Image,
        camera: &Camera,
        environment: Environment,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.render_options(
            gpu,
            output,
            camera,
            environment,
            Options::new(true, streamline::Quality::Dlaa, 1, gpu.settings.max_bounces)?,
        )
    }

    fn render_options(
        &self,
        gpu: &mut Gpu,
        output: &Image,
        camera: &Camera,
        environment: Environment,
        options: Options,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.render_profiled(gpu, output, camera, environment, options, None)?;
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "与实际draw输入共用宿主提交及完成边界"
    )]
    fn render_profiled(
        &self,
        gpu: &mut Gpu,
        output: &Image,
        camera: &Camera,
        environment: Environment,
        options: Options,
        queries: Option<[vulkan::native::TimestampQueries; 2]>,
    ) -> Result<[u64; 2], Box<dyn std::error::Error>> {
        let raw = output.raw(&gpu.context)?;
        let mut cpu_ns = 0;
        let mut markers = Vec::new();
        // SAFETY: 测试独占 graphics/compute queue，并在回读或更新前完成命令和 Flight。
        unsafe {
            let pool = self.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(self.family),
                None,
            )?;
            let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                let command = self.device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1)
                        .level(vk::CommandBufferLevel::PRIMARY),
                )?[0];
                self.device
                    .begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())?;
                if let Some([query, _]) = queries {
                    let mut before = Commands::new(&gpu.context)?;
                    before.reset_timestamps(query)?;
                    before.write_timestamp(query, 0)?;
                    let before = before.finish(&mut gpu.context)?;
                    markers.push(before.encode(&mut gpu.context, command.as_raw())?);
                }
                let started = std::time::Instant::now();
                gpu.draw(
                    command.as_raw(),
                    raw.image,
                    raw.view,
                    [raw.extent[0], raw.extent[1]],
                    camera,
                    environment,
                    options,
                )?;
                cpu_ns = started.elapsed().as_nanos() as u64;
                if let Some([_, query]) = queries {
                    let mut after = Commands::new(&gpu.context)?;
                    after.reset_timestamps(query)?;
                    after.write_timestamp(query, 0)?;
                    let after = after.finish(&mut gpu.context)?;
                    markers.push(after.encode(&mut gpu.context, command.as_raw())?);
                }
                self.device.end_command_buffer(command)?;
                self.device.queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&[command])],
                    vk::Fence::null(),
                )?;
                Ok(())
            })();
            if let Err(error) = self.device.queue_wait_idle(self.queue) {
                if error == vk::Result::ERROR_DEVICE_LOST {
                    self.device.destroy_command_pool(pool, None);
                } else {
                    self.retained.set(true);
                }
                return Err(error.into());
            }
            if result.is_err() {
                gpu.display_timeline.cancel();
            }
            for marker in markers {
                gpu.context.complete(marker)?;
            }
            gpu.complete_previous()?;
            self.device.destroy_command_pool(pool, None);
            result?;
        }
        let gpu_ns = if let Some([before, after]) = queries {
            let start = before.read(&gpu.context)?.ok_or("完成后timestamp未ready")?[0];
            let end = after.read(&gpu.context)?.ok_or("完成后timestamp未ready")?[0];
            before.elapsed_ns(&gpu.context, start, end)?
        } else {
            0
        };
        Ok([cpu_ns, gpu_ns])
    }
}

fn camera() -> Camera {
    Camera {
        origin: [0.; 4],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    }
}

fn image(gpu: &mut Gpu) -> Result<Image, Error> {
    let image = Image::new(&mut gpu.context, [32, 32, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(image.into());
    Ok(image)
}

fn center(pixels: &[u8]) -> &[u8] {
    let offset = (16 * 32 + 16) * 4;
    &pixels[offset..offset + 4]
}

#[test]
#[ignore = "真实Vulkan RT；实时固定双样本，离线接受原批大小，覆盖 Hit Group 和兼容入口"]
fn hardware_realtime_quality_and_offline_batches() -> Result<(), Box<dyn std::error::Error>> {
    for hit_groups in [false, true] {
        let host = Host::create(false, hit_groups)?;
        let mut session = host.gpu()?;
        let gpu = session.gpu();
        let output = image(gpu)?;
        let mut mesh = Mesh::default();
        add_quad(&mut mesh, 2., [0.5; 2], [1.; 3], 1.5);
        gpu.set_scene(&mesh)?;
        for (mode, requested, expected) in [
            (prime_scene::RenderMode::Realtime, 1, 2),
            (prime_scene::RenderMode::Offline, 5, 5),
            (prime_scene::RenderMode::Realtime, 5, 2),
        ] {
            gpu.set_rendering(mode)?;
            let first = gpu.absolute_sample;
            host.render_options(
                gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, requested, 8)?,
            )?;
            assert_eq!(gpu.absolute_sample - first, expected);
            assert_eq!(gpu.accumulated_samples(), expected);
        }
        assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    }
    Ok(())
}

#[test]
#[ignore = "需要 EXT SER；验证重排不改变相同路径样本的 HDR 与宿主呈现"]
fn hardware_pt_ser_matches_plain() -> Result<(), Box<dyn std::error::Error>> {
    const HDR_TOLERANCE: f32 = 1e-6;
    struct Frame {
        hdr: Vec<u8>,
        display: Vec<u8>,
        params: Vec<u8>,
        counts: Vec<u8>,
        prefix: Vec<u8>,
    }
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    assert!(gpu.context.capabilities().invocation_reorder);
    let atlas = [255, 0, 0, 127, 128, 200, 80, 255];
    gpu.set_atlas(2, 1, &atlas)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.75, 0.5], [1.; 3], 1.5);
    add_quad(&mut mesh, 1., [0.25, 0.5], [1.; 3], 0.7);
    let start = mesh.quads.len();
    add_quad(&mut mesh, 0.8, [0.75, 0.5], [0.3, 0.7, 1.], 0.3);
    for quad in &mut mesh.quads[start..] {
        quad.translate([0.4, 0., 0.]);
    }
    gpu.set_scene(&mesh)?;
    let mut view = View::new(&mut gpu.context, [32, 32], false, &mut gpu.bloom_filter)?;
    view.accumulation = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    view.counts = Buffer::new(&mut gpu.context, 32 * 32 * 4, true)?;
    view.prefix = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.view = Some(view);
    // 对照开始前完成 SkyView/Aerial 首次生成，三个受检帧共用已完成的环境资源。
    host.render(&mut gpu, &image, &camera())?;
    let first_sample = gpu.absolute_sample;
    let mut reference: Option<Frame> = None;
    let mut unchanged = true;
    for (index, invocation_reorder) in [false, false, true, false].into_iter().enumerate() {
        // 通过生产设置入口切换，只重置计数以比较同一绝对样本序列。
        gpu.set_ser(invocation_reorder)?;
        gpu.invalidate()?;
        gpu.test_restart_sample_sequence(first_sample)?;
        let display = host.render(&mut gpu, &image, &camera())?;
        let mut hdr = vec![0; 32 * 32 * 16];
        gpu.view
            .as_ref()
            .unwrap()
            .accumulation
            .read(&mut gpu.context, 0, &mut hdr)?;
        let view = gpu.view.as_ref().unwrap();
        let mut inputs = Vec::new();
        for buffer in [&view.params, &view.counts, &view.prefix] {
            let mut bytes = vec![0; buffer.len() as usize];
            buffer.read(&mut gpu.context, 0, &mut bytes)?;
            inputs.push(bytes);
        }
        let [params, counts, prefix]: [_; 3] = inputs.try_into().unwrap();
        assert!(
            counts
                .as_chunks::<4>()
                .0
                .iter()
                .all(|word| u32::from_le_bytes(*word) == 2)
        );
        if let Ok(directory) = std::env::var("PRIMARY_RT_SER_CAPTURE") {
            let directory = std::path::Path::new(&directory);
            std::fs::create_dir_all(directory)?;
            std::fs::write(
                directory.join(format!("{index}-{invocation_reorder}.hdr")),
                &hdr,
            )?;
            for (name, bytes) in [
                ("params", &params),
                ("counts", &counts),
                ("prefix", &prefix),
                ("display", &display),
            ] {
                std::fs::write(
                    directory.join(format!("{index}-{invocation_reorder}.{name}")),
                    bytes,
                )?;
            }
        }
        assert!(
            hdr.as_chunks::<16>()
                .0
                .iter()
                .any(|pixel| pixel[..12].iter().any(|&v| v != 0))
        );
        // 首个普通程序执行预热；正式比较包含 plain→SER→plain 的运行时切换。
        if index == 0 {
            continue;
        }
        if let Some(expected) = &reference {
            assert_eq!(params, expected.params);
            assert_eq!(counts, expected.counts);
            let prefix_error = prefix
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.prefix.as_chunks::<4>().0)
                .map(|(a, b)| {
                    let a = f32::from_le_bytes(*a);
                    let b = f32::from_le_bytes(*b);
                    assert!(a.is_finite() && b.is_finite());
                    (a - b).abs() / (1. + a.abs().max(b.abs()))
                })
                .fold(0f32, f32::max);
            assert!(
                prefix_error <= HDR_TOLERANCE,
                "K1 prefix scaled error={prefix_error}"
            );
            let mut count = 0;
            let mut max_error = 0.0f32;
            for (actual, expected) in hdr
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.hdr.as_chunks::<4>().0)
            {
                let a = f32::from_le_bytes(*actual);
                let b = f32::from_le_bytes(*expected);
                assert!(a.is_finite() && b.is_finite());
                count += usize::from(actual != expected);
                max_error = max_error.max((a - b).abs() / b.abs().max(1.));
            }
            let display_error = display
                .iter()
                .zip(&expected.display)
                .map(|(a, b)| f32::from(a.abs_diff(*b)) / 255.)
                .fold(0.0f32, f32::max);
            eprintln!(
                "SER={invocation_reorder} 不同 HDR 通道={count}，最大 scaled error={max_error}，呈现误差={display_error}"
            );
            // 同字节程序重建在本机可有 3 ULP 差异，沿用 PT 的 1e-6 数值约定。
            unchanged &= max_error <= HDR_TOLERANCE && display_error <= HDR_TOLERANCE;
        } else {
            reference = Some(Frame {
                hdr,
                display,
                params,
                counts,
                prefix,
            });
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    assert!(unchanged, "程序切换改变了相同绝对样本序列的 HDR 或呈现");
    Ok(())
}

#[test]
#[ignore = "需要 EXT SER；验证运行时开关保留累积样本"]
fn hardware_pt_ser_toggle_preserves_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut session = host.gpu_requested(false)?;
    let mut gpu = session.gpu();
    assert!(!gpu.invocation_reorder);
    let image = image(&mut gpu)?;
    host.render(&mut gpu, &image, &camera())?;
    for (enabled, expected_samples) in [(true, 4), (false, 6), (true, 8)] {
        let started = std::time::Instant::now();
        gpu.set_ser(enabled)?;
        eprintln!("mod SER={enabled} 切换准备耗时={:?}", started.elapsed());
        assert_eq!(gpu.invocation_reorder, enabled);
        assert!(gpu.context.capabilities().invocation_reorder);
        assert_eq!(
            gpu.view.as_ref().unwrap().progress.samples(),
            expected_samples - 2
        );
        host.render(&mut gpu, &image, &camera())?;
        let view = gpu.view.as_ref().unwrap();
        assert_eq!(view.progress.samples(), expected_samples);
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要硬件 RT；验证逻辑设备未启用 SER 时保持普通程序"]
fn hardware_pt_ser_unavailable_uses_plain() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    assert!(!gpu.context.capabilities().invocation_reorder);
    gpu.set_ser(true)?;
    assert!(!gpu.invocation_reorder);
    let image = image(&mut gpu)?;
    host.render(&mut gpu, &image, &camera())?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 2);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证宿主呈现的上下方向"]
fn hardware_pt_host_image_y() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    for (y, tint) in [(0.7, [1., 0., 0.]), (-0.7, [0., 1., 0.])] {
        let start = mesh.quads.len();
        add_quad(&mut mesh, 1., [0.5; 2], tint, 0.5);
        for quad in &mut mesh.quads[start..] {
            quad.translate([0., y, 0.]);
        }
    }
    gpu.set_scene(&mesh)?;
    let pixels = host.render(&mut gpu, &image, &camera())?;
    let lower = &pixels[(4 * 32 + 16) * 4..][..4];
    let upper = &pixels[(27 * 32 + 16) * 4..][..4];
    assert!(lower[1] > lower[0] + 30, "宿主下方应为绿色：{lower:?}");
    assert!(upper[0] > upper[1] + 30, "宿主上方应为红色：{upper:?}");
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证 alpha cutout 与删除透明表面等价"]
fn hardware_pt_alpha_cutout() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    gpu.set_atlas(2, 1, &[255, 0, 0, 127, 160, 200, 240, 255])?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.75, 0.5], [1.; 3], 2.);
    gpu.set_scene(&mesh)?;
    let first_sample = gpu.absolute_sample;
    let reference = host.render(&mut gpu, &image, &camera())?;
    add_quad(&mut mesh, 1., [0.25, 0.5], [1.; 3], 1.);
    gpu.set_scene(&mesh)?;
    gpu.test_restart_sample_sequence(first_sample)?;
    let cutout = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(center(&reference), center(&cutout));
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证 atlas 与 callable 一起发布及失败保留旧资源"]
fn hardware_pt_material_reload() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [1.; 3], 2.);
    gpu.set_scene(&mesh)?;
    let white_sample = gpu.absolute_sample;
    let white = host.render(&mut gpu, &image, &camera())?;
    gpu.set_atlas(1, 1, &[0, 0, 0, 255])?;
    let black_sample = gpu.absolute_sample;
    let black = host.render(&mut gpu, &image, &camera())?;
    assert_ne!(center(&white), center(&black));
    assert!(gpu.set_atlas(2, 1, &[255; 4]).is_err());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let unavailable =
        shader_build::RuntimeCompiler::new(root.join("../../target/reload-error"), &[], &[])?;
    let compiler = std::mem::replace(&mut gpu.compiler, unavailable);
    assert!(matches!(
        gpu.set_atlas(1, 1, &[255; 4]),
        Err(Error::Shader(shader_build::Error::Compile { .. }))
    ));
    gpu.compiler = compiler;
    gpu.restart_accumulation()?;
    // 重启累积保留绝对随机序列；资源等价对照必须使用相同样本输入。
    gpu.absolute_sample = black_sample;
    let retained = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(black, retained);
    gpu.set_atlas(1, 1, &[255; 4])?;
    gpu.absolute_sample = white_sample;
    let restored = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(white, restored);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；失败批次只取消自己且允许下一帧继续"]
fn hardware_pt_failed_recording_preserves_other_pending() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let output = image(&mut gpu)?;
    let retained = Buffer::new(&mut gpu.context, 16, true)?;
    gpu.test_roots.push(retained.into());
    let mut commands = Commands::new(&gpu.context)?;
    commands.clear_buffer(retained)?;
    let pending = commands.finish(&mut gpu.context)?;
    assert!(
        gpu.record(
            [32, 32],
            &camera(),
            Environment::default(),
            Binding::StorageBuffer(retained),
        )
        .is_err()
    );
    assert!(retained.write(&mut gpu.context, 0, &[0; 16]).is_err());
    gpu.context.cancel(pending)?;
    retained.write(&mut gpu.context, 0, &[0; 16])?;
    host.render(&mut gpu, &output, &camera())?;
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；材质程序不能保留已退役的场景工作集"]
fn hardware_pt_material_program_releases_retired_scene() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [1.; 3], 2.);
    gpu.set_scene(&mesh)?;
    let retired = gpu.scene.top;
    gpu.set_atlas(1, 1, &[0, 0, 0, 255])?;
    gpu.set_scene(&Mesh::default())?;
    assert!(
        Bindings::new(&mut gpu.context, &[Binding::Acceleration(retired)]).is_err(),
        "程序模板钉住了旧场景资源"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证 atlas 与 tint 使用相同线性颜色约定"]
fn hardware_pt_linear_tint() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [1.; 3], 2.);
    gpu.set_atlas(1, 1, &[128, 192, 64, 255])?;
    gpu.set_scene(&mesh)?;
    let sample_checkpoint = gpu.absolute_sample;
    let texture = host.render(&mut gpu, &image, &camera())?;
    gpu.set_atlas(1, 1, &[255; 4])?;
    for quad in &mut mesh.quads {
        quad.set_tint([128. / 255., 192. / 255., 64. / 255.]);
    }
    gpu.set_scene(&mesh)?;
    gpu.test_restart_sample_sequence(sample_checkpoint)?;
    let tint = host.render(&mut gpu, &image, &camera())?;
    assert!(
        center(&texture)
            .iter()
            .zip(center(&tint))
            .all(|(a, b)| a.abs_diff(*b) <= 1),
        "atlas {:?}，tint {:?}",
        center(&texture),
        center(&tint)
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证跨帧合批保持绝对样本身份"]
fn hardware_pt_sample_batching() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [0.5, 1., 0.5], 0.7);
    gpu.set_scene(&mesh)?;
    gpu.set_rendering(prime_scene::RenderMode::Offline)?;
    gpu.settings.samples = 8;
    let first = gpu.absolute_sample;
    let batched = host.render(&mut gpu, &image, &camera())?;
    gpu.restart_accumulation()?;
    gpu.test_restart_sample_sequence(first)?;
    gpu.settings.samples = 1;
    let mut split = Vec::new();
    for _ in 0..8 {
        split = host.render(&mut gpu, &image, &camera())?;
    }
    assert_eq!(batched, split);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证主查询与阴影候选使用同一 alpha 边界"]
fn hardware_pt_ray_query_alpha() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.25, 0.5], [1.; 3], 1.);
    add_quad(&mut mesh, 2., [0.75, 0.5], [1.; 3], 1.);
    gpu.set_scene(&mesh)?;
    for (alpha, expected) in [(127, [0, 1]), (128, [1, 1])] {
        gpu.set_atlas(2, 1, &[255, 0, 0, alpha, 255, 255, 255, 255])?;
        let output = Buffer::new(&mut gpu.context, 16, true)?;
        let mut descriptors = vec![
            Binding::Acceleration(gpu.scene.top),
            Binding::StorageBuffer(gpu.scene.quads),
            Binding::SampledImage(gpu.atlas),
            Binding::Sampler(gpu.nearest),
            Binding::StorageBuffer(output),
        ];
        descriptors.resize(27, Binding::StorageBuffer(output));
        descriptors.extend([
            Binding::StorageBuffer(gpu.material_gpu.table),
            Binding::SampledImage(gpu.material_gpu.mers),
            Binding::SampledImage(gpu.material_gpu.auxiliary),
        ]);
        descriptors.resize(36, Binding::StorageBuffer(output));
        descriptors.push(Binding::StorageBuffer(gpu.scene.instance_quad_bases));
        let bindings = Bindings::new(&mut gpu.context, &descriptors)?;
        let programs = programs(&[], false, gpu.generation.as_ref().unwrap().hit);
        let reference = vulkan::native::RayTracingPipeline::new(
            &mut gpu.context,
            bindings,
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/visibility_reference.spv")),
                    c"reference_main",
                ),
                programs.miss,
                programs.closest_hit,
            ],
            programs.any_hit,
            &[],
        )?;
        let query = ComputePipeline::new(
            &mut gpu.context,
            bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/visibility.spv")),
                c"visibility_main",
            ),
        )?;
        let mut commands = Commands::new(&gpu.context)?;
        commands.trace(reference, [2, 1])?;
        commands.barrier();
        commands.dispatch(query, [1, 1, 1])?;
        commands.barrier();
        commands
            .finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        let mut bytes = [0u8; 16];
        output.read(&mut gpu.context, 0, &mut bytes)?;
        let actual: [u32; 4] = std::array::from_fn(|i| {
            u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        assert_eq!(&actual[..2], &expected, "TraceRay alpha={alpha}");
        assert_eq!(&actual[2..], &expected, "RayQuery alpha={alpha}");
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证相机变化不会保留旧视图的累积"]
fn hardware_pt_camera_resets_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [0., 1., 0.], 2.);
    gpu.set_scene(&mesh)?;
    host.render(&mut gpu, &image, &camera())?;
    let mut shifted = camera();
    shifted.origin[0] = 100.;
    let first_sample = gpu.absolute_sample;
    let changed = host.render(&mut gpu, &image, &shifted)?;
    gpu.restart_accumulation()?;
    gpu.test_restart_sample_sequence(first_sample)?;
    let fresh = host.render(&mut gpu, &image, &shifted)?;
    assert_eq!(changed, fresh);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证太阳 NEE 阴影与 alpha 覆盖一致"]
fn hardware_pt_sun_cutout_shadow() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let environment = Environment {
        sun_direction: [0.28f32.sqrt(), 0.6, -0.6],
        ..Default::default()
    };
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 3., [0.75, 0.5], [1.; 3], 5.);
    gpu.set_scene(&mesh)?;
    let first_sample = gpu.absolute_sample;
    let lit = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let start = mesh.quads.len();
    add_quad(&mut mesh, 2., [0.25, 0.5], [1.; 3], 0.5);
    for quad in &mut mesh.quads[start..] {
        quad.translate([0.95, 1., 0.]);
    }
    gpu.set_scene(&mesh)?;
    gpu.set_atlas(2, 1, &[255, 255, 255, 127, 255, 255, 255, 255])?;
    gpu.test_restart_sample_sequence(first_sample)?;
    let cutout = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(center(&lit), center(&cutout));
    gpu.set_atlas(2, 1, &[255; 8])?;
    gpu.test_restart_sample_sequence(first_sample)?;
    let shadow = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert!(
        (0..3).all(|i| center(&lit)[i] > center(&shadow)[i] + 8),
        "太阳直照 {:?}，阴影 {:?}",
        center(&lit),
        center(&shadow)
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证天空 azimuth 使用世界太阳方向"]
fn hardware_pt_sky_direction() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut environment = Environment {
        sun_direction: [0., 0.6, 0.8],
        ..Default::default()
    };
    let forward = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    environment.sun_direction = [0., 0.6, -0.8];
    let backward = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 2);
    assert_ne!(center(&forward), center(&backward), "天空应随太阳方向变化");
    environment.sun_direction = [0.8, 0.6, 0.];
    let mut rotated = camera();
    rotated.forward = [1., 0., 0., 0.];
    rotated.right = [0., 0., -1., 0.];
    gpu.absolute_sample = 0; // 旋转对照使用与首帧相同的亚像素样本。
    let rotated = host.render_environment(&mut gpu, &image, &rotated, environment)?;
    assert!(
        forward
            .iter()
            .zip(rotated)
            .all(|(&a, b)| a.abs_diff(b) <= 1)
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证主射线距离驱动 Aerial 散射"]
fn hardware_pt_aerial_distance() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.settings.max_bounces = 1;
    let image = image(&mut gpu)?;
    let mut narrow = camera();
    narrow.right[0] = 0.0001;
    narrow.up[1] = 0.0001;
    let mut pixels = Vec::new();
    for distance in [1., 10_000.] {
        let mut mesh = Mesh::default();
        add_quad(&mut mesh, distance, [0.5; 2], [0.; 3], 10.);
        gpu.set_scene(&mesh)?;
        pixels.push(host.render(&mut gpu, &image, &narrow)?);
    }
    assert!(
        center(&pixels[1])[2] > center(&pixels[0])[2] + 10,
        "近处 {:?}，远处 {:?}",
        center(&pixels[0]),
        center(&pixels[1])
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证曝光只改变呈现并保留 PT 和大气历史"]
fn hardware_pt_exposure_preserves_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let mut view = View::new(&mut gpu.context, [32, 32], false, &mut gpu.bloom_filter)?;
    view.accumulation = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.view = Some(view);
    let mut environment = Environment::default();
    let initial = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let atmosphere = *gpu.atmosphere.parameters().words();
    let mapping = *gpu.atmosphere.sky_mapping();
    environment.exposure_ev = 2.;
    let exposed = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 4);
    assert_eq!(gpu.atmosphere.parameters().words(), &atmosphere);
    assert_eq!(gpu.atmosphere.sky_mapping(), &mapping);
    let brightness = |pixels: &[u8]| {
        center(pixels)[..3]
            .iter()
            .map(|&v| u32::from(v))
            .sum::<u32>()
    };
    assert!(brightness(&exposed) > brightness(&initial));
    environment.exposure_ev = -2.;
    let dark = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 6);
    assert!(brightness(&dark) < brightness(&initial));
    let mut exposed_hdr = vec![0; 32 * 32 * 16];
    gpu.view
        .as_ref()
        .unwrap()
        .accumulation
        .read(&mut gpu.context, 0, &mut exposed_hdr)?;
    gpu.restart_accumulation()?;
    gpu.test_restart_sample_sequence(0)?;
    environment.exposure_ev = 0.;
    for _ in 0..3 {
        host.render_environment(&mut gpu, &image, &camera(), environment)?;
    }
    let mut reference_hdr = vec![0; exposed_hdr.len()];
    gpu.view
        .as_ref()
        .unwrap()
        .accumulation
        .read(&mut gpu.context, 0, &mut reference_hdr)?;
    assert!(
        exposed_hdr == reference_hdr,
        "曝光不能改变同一绝对样本序列的 HDR"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn exposure_state(gpu: &mut Gpu) -> Result<[u32; 4], Box<dyn std::error::Error>> {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../tests/shaders/shaders/exposure.slang");
    let code = gpu.compiler.compile(&shader_build::Recipe {
        source: source.to_str().ok_or("曝光回读shader路径不是UTF-8")?,
        entry: "readback_main",
        stage: "compute",
        output: "exposure.readback.spv",
        target: shader_build::Target::Spirv("spirv_1_4"),
        args: &["-fp-mode", "precise"],
    })?;
    let output = Buffer::new(&mut gpu.context, 16, true)?;
    let state = gpu
        .view
        .as_ref()
        .ok_or("曝光回读需要已创建视图")?
        .presentation
        .display
        .state();
    let bindings = Bindings::new(
        &mut gpu.context,
        &[
            Binding::StorageBuffer(state),
            Binding::StorageBuffer(output),
        ],
    )?;
    let pipeline = ComputePipeline::new(
        &mut gpu.context,
        bindings,
        RayShader {
            spirv: code.bytes(),
            entry: c"readback_main",
        },
    )?;
    gpu.test_roots.push(pipeline.into());
    let mut commands = Commands::new(&gpu.context)?;
    commands.barrier();
    commands.dispatch(pipeline, [1; 3])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
        .submit_and_wait(&mut gpu.context)?;
    let mut bytes = [0; 16];
    output.read(&mut gpu.context, 0, &mut bytes)?;
    Ok(std::array::from_fn(|i| {
        u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
    }))
}

fn exposure_hdr(gpu: &mut Gpu) -> Result<Vec<u8>, Error> {
    let buffer = gpu.view.as_ref().expect("测试已创建视图").accumulation;
    let mut bytes = vec![0; buffer.len() as usize];
    buffer.read(&mut gpu.context, 0, &mut bytes)?;
    Ok(bytes)
}

fn accumulated_rgb(gpu: &mut Gpu) -> Result<Vec<[f32; 3]>, Error> {
    Ok(exposure_hdr(gpu)?
        .as_chunks::<16>()
        .0
        .iter()
        .map(|pixel| {
            std::array::from_fn(|i| f32::from_le_bytes(pixel[i * 4..i * 4 + 4].try_into().unwrap()))
        })
        .collect())
}

fn exposure_view(gpu: &mut Gpu) -> Result<(), Error> {
    let mut view = View::new(&mut gpu.context, [32, 32], false, &mut gpu.bloom_filter)?;
    view.accumulation = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.view = Some(view);
    Ok(())
}

fn freeze_exposure_clock(gpu: &mut Gpu) {
    // 宿主时钟固定为零：elapsed 对未来原点饱和，避免GPU耗时影响适应断言。
    gpu.display_origin = std::time::Instant::now() + std::time::Duration::from_secs(3600);
}

#[test]
#[ignore = "真实 Vulkan RT；太阳盘与散射天空共享场景线性太阳 EV"]
fn hardware_pt_sun_ev_doubles_disk_and_scattered_sky() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    let camera = Camera {
        forward: [0., 1., 0., 0.],
        right: [0.014, 0., 0., 0.],
        up: [0., 0., -0.014, 0.],
        ..camera()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 4)?;
    host.render_options(&mut gpu, &image, &camera, Environment::default(), options)?;
    let baseline = accumulated_rgb(&mut gpu)?;
    gpu.set_lighting_options(1., 0.)?;
    gpu.absolute_sample = 0; // 同一组相机射线只改变太阳强度。
    host.render_options(&mut gpu, &image, &camera, Environment::default(), options)?;
    let brighter = accumulated_rgb(&mut gpu)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 2);
    let peak = baseline.iter().map(|pixel| pixel[0]).fold(0., f32::max);
    assert!(peak > 1000., "视野包含太阳盘，峰值 {peak}");
    assert!(
        baseline
            .iter()
            .any(|pixel| (f32::MIN_POSITIVE..0.01 * peak).contains(&pixel[0])),
        "视野包含散射天空"
    );
    for (before, after) in baseline
        .into_iter()
        .flatten()
        .zip(brighter.into_iter().flatten())
    {
        assert!(
            (after - 2. * before).abs() <= 1e-5 * (1. + before),
            "太阳 EV 线性放大 {before} -> {after}"
        );
    }
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn diffraction_target(
    host: &Host,
    gpu: &mut Gpu,
    [width, height]: [usize; 2],
) -> Result<Image, Box<dyn std::error::Error>> {
    let target = Image::new(
        &mut gpu.context,
        [width as u32, height as u32, 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(target.into());
    host.render_options(
        gpu,
        &target,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Dlaa, 1, 4)?,
    )?;
    Ok(target)
}

fn present_diffraction(
    gpu: &mut Gpu,
    target: Image,
    pixels: &[[f32; 4]],
) -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
    let view = gpu.view.as_ref().unwrap();
    let bytes = bytemuck::cast_slice(pixels);
    let source = Buffer::upload(&mut gpu.context, bytes)?;
    source.copy_prefix_to(&mut gpu.context, &view.output, bytes.len() as u64)?;
    view.presentation
        .record(
            &mut gpu.context,
            Binding::StorageImage(target),
            HdrSource::Raw,
            false,
        )?
        .submit_and_wait(&mut gpu.context)?;
    let hdr = view.presentation.hdr().download(&mut gpu.context)?;
    let display = target.download(&mut gpu.context)?;
    Ok((hdr, display))
}

fn write_diffraction_png(
    path: &std::path::Path,
    display: &[u8],
    width: usize,
    height: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut png = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    png.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = png.write_header()?;
    let mut stream = writer.stream_writer()?;
    for row in display.chunks_exact(width * 4).rev() {
        stream.write_all(row)?;
    }
    stream.finish()?;
    writer.finish()?;
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件，在 Prime PT 的实际呈现链上检查快速圆孔点源"]
fn hardware_pt_aperture_fast_point() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let [w, h] = [2191usize, 1933];
    let [cx, cy] = [1224, 968];
    let target = diffraction_target(&host, gpu, [w, h])?;
    let mut pixels = vec![[0., 0., 0., 1.]; w * h];
    let power = 2_097_152.;
    pixels[cy * w + cx] = [power, power, power, 1.];
    let (hdr, display) = present_diffraction(gpu, target, &pixels)?;
    let mut flux = [0f64; 3];
    for &word in hdr.as_chunks::<16>().0 {
        let sample: [f32; 4] = bytemuck::cast(word);
        assert_eq!(sample[3], 1.);
        for c in 0..3 {
            assert!(sample[c] >= 0.);
            flux[c] += f64::from(sample[c]);
        }
    }
    assert!(
        flux.iter()
            .all(|v| (v / f64::from(power) - 1.).abs() < 2e-5)
    );
    let center: [f32; 4] = bytemuck::cast(hdr.as_chunks::<16>().0[cy * w + cx]);
    let [r, g, b, _] = center;
    let near = [r, g, b].map(|value| value / power);
    assert!(0.82 < near[0] && near[0] < near[1] && near[1] < near[2] && near[2] < 0.87);
    eprintln!("快速圆形点光源 RGB 通量 {flux:?}，中心占比 {near:?}");
    if let Some(path) = std::env::var_os("PRIME_PT_DIFFRACTION_OUTPUT") {
        let path = std::path::Path::new(&path);
        std::fs::create_dir_all(path)?;
        write_diffraction_png(&path.join("fast-point-display.png"), &display, w, h)?;
        std::fs::write(path.join("fast-point-linear.rgba32f"), hdr)?;
        std::fs::write(
            path.join("fast-point.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "extent":[w,h], "source":[cx,cy], "origin":"top-left", "power":power, "flux":flux, "center_fraction":near
            }))?,
        )?;
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件，核对相机光圈的点扩散函数"]
fn hardware_pt_aperture_camera_point() -> Result<(), Box<dyn std::error::Error>> {
    aperture_point(
        fft_bloom::Aperture::Camera,
        "camera-point",
        [[22, 9], [59, 25], [119, 46], [292, 130]],
    )
}

#[test]
#[ignore = "需要 Vulkan 硬件，核对导入瞳孔的点扩散函数"]
fn hardware_pt_aperture_pupil_point() -> Result<(), Box<dyn std::error::Error>> {
    aperture_point(
        fft_bloom::Aperture::Pupil,
        "pupil-point",
        [[19, 14], [55, 32], [128, 4], [271, 170]],
    )
}

fn aperture_point(
    shape: fft_bloom::Aperture,
    stem: &str,
    probes: [[usize; 2]; 4],
) -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    gpu.set_lens_bloom(Some(shape.into()));
    let [width, height] = [2191usize, 1933];
    let target = diffraction_target(&host, gpu, [width, height])?;
    let [cx, cy] = [1224usize, 968];
    let power = 2_097_152.0f32;
    let mut pixels = vec![[0., 0., 0., 1.]; width * height];
    pixels[cy * width + cx] = [power, power, power, 1.];
    let (hdr, display) = present_diffraction(gpu, target, &pixels)?;
    let kernel = shape.kernel()?;
    let [side, _] = kernel.extent().map(|n| n as usize);
    let mut flux = [0.0f64; 3];
    let mut peak_error = 0.0f32;
    for (i, &word) in hdr.as_chunks::<16>().0.iter().enumerate() {
        let value: [f32; 4] = bytemuck::cast(word);
        let x = (i % width + side / 2).checked_sub(cx);
        let y = (i / width + side / 2).checked_sub(cy);
        assert_eq!(value[3], 1.0);
        for (c, weights) in kernel.channels().iter().enumerate() {
            let expected = match (x, y) {
                (Some(x), Some(y)) if x < side && y < side => power * weights[y * side + x],
                _ => 0.0,
            };
            assert!(value[c].is_finite());
            peak_error = peak_error.max((value[c] - expected).abs());
            flux[c] += f64::from(value[c]);
        }
    }
    eprintln!("{shape:?} 点光源 RGB 通量 {flux:?}，最大逐像素绝对误差 {peak_error:e}");
    // 全图绝对误差限为输入通量的半个 ULP。不同方位的翼部另按当地亮度检查相对误差。
    assert!(peak_error <= 0.5 * f32::EPSILON * power);
    for [dx, dy] in probes {
        let tip: [f32; 4] = bytemuck::cast(hdr.as_chunks::<16>().0[(cy + dy) * width + cx + dx]);
        for (c, weights) in kernel.channels().iter().enumerate() {
            let reference = power * weights[(side / 2 + dy) * side + side / 2 + dx];
            assert!(
                (tip[c] / reference - 1.).abs() < 0.03,
                "翼部 {dx},{dy} 通道 {c}: {} / {reference}",
                tip[c]
            );
        }
    }
    assert!(
        flux.iter()
            .all(|v| (v / f64::from(power) - 1.0).abs() < 1e-5)
    );
    let rgba = display.as_chunks::<4>().0;
    assert_eq!(rgba[(height - 1 - cy) * width + cx], [255; 4]);
    assert!(rgba.iter().any(|&[r, _, b, _]| r.abs_diff(b) > 10));
    if let Some(path) = std::env::var_os("PRIME_PT_DIFFRACTION_OUTPUT") {
        let path = std::path::Path::new(&path);
        std::fs::create_dir_all(path)?;
        write_diffraction_png(
            &path.join(format!("{stem}-display.png")),
            &display,
            width,
            height,
        )?;
        std::fs::write(path.join(format!("{stem}-linear.rgba32f")), hdr)?;
        std::fs::write(
            path.join(format!("{stem}.json")),
            serde_json::to_vec_pretty(&serde_json::json!({
                "extent":[width,height],"source":[cx,cy],"power":power,"origin":"top-left","flux":flux,"max_error":peak_error
            }))?,
        )?;
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 光追设备，检查泛光开关的呈现、内存回收和原视图恢复"]
fn hardware_pt_aperture_toggle() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    gpu.set_lens_bloom(Some(fft_bloom::Aperture::Camera.into()));
    let [w, h] = [193, 129];
    let target = diffraction_target(&host, gpu, [w, h])?;
    let mut pixels = vec![[0., 0., 0., 1.]; w * h];
    pixels[h / 2 * w + w / 2] = [4096., 2048., 1024., 0.5];
    let (on, display) = present_diffraction(gpu, target, &pixels)?;
    let accumulation = gpu.view.as_ref().unwrap().accumulation;
    gpu.collect_resources()?;
    let active_bytes = gpu.context.allocated_bytes();
    gpu.set_lens_bloom(Some(fft_bloom::Aperture::Pupil.into()));
    host.render_options(
        gpu,
        &target,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Dlaa, 2, 4)?,
    )?;
    let (rounded, _) = present_diffraction(gpu, target, &pixels)?;
    let center = h / 2 * w + w / 2;
    let rounded_center: [f32; 4] = bytemuck::cast(rounded.as_chunks::<16>().0[center]);
    let original_center: [f32; 4] = bytemuck::cast(on.as_chunks::<16>().0[center]);
    for (shape, center_value) in [
        (fft_bloom::Aperture::Camera, original_center),
        (fft_bloom::Aperture::Pupil, rounded_center),
    ] {
        let psf = shape.rgb32le().as_chunks::<12>().0;
        let green = f32::from_le_bytes(psf[psf.len() / 2].as_chunks::<4>().0[1]);
        assert!((center_value[1] - 2048. * green).abs() < 0.001);
    }
    gpu.collect_resources()?;
    assert!(
        gpu.context.allocated_bytes() < active_bytes + (1 << 20),
        "同一时刻只保留一个完整工作区"
    );
    gpu.set_lens_bloom(Some(LensMode::FastCircular));
    host.render_options(
        gpu,
        &target,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Dlaa, 2, 4)?,
    )?;
    let (fast, _) = present_diffraction(gpu, target, &pixels)?;
    assert_ne!(fast, rounded);
    gpu.collect_resources()?;
    assert!(
        gpu.context.allocated_bytes() < active_bytes,
        "快速模式须回收 FFT 工作区"
    );
    gpu.set_lens_bloom(None);
    gpu.collect_resources()?;
    assert!(gpu.context.allocated_bytes() < active_bytes);
    let (off, bypass) = present_diffraction(gpu, target, &pixels)?;
    assert_eq!(off, bytemuck::cast_slice::<_, u8>(&pixels));
    assert_ne!(display, bypass);
    gpu.set_lens_bloom(Some(fft_bloom::Aperture::Camera.into()));
    host.render_options(
        gpu,
        &target,
        &camera(),
        Environment::default(),
        Options::new(false, streamline::Quality::Dlaa, 1, 4)?,
    )?;
    assert_eq!(accumulation, gpu.view.as_ref().unwrap().accumulation);
    let restored = present_diffraction(gpu, target, &pixels)?;
    assert_eq!((on, display), restored);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件，核对快速圆形泛光在太阳盘周围的衰减与圆度"]
fn hardware_pt_aperture_fast_sun() -> Result<(), Box<dyn std::error::Error>> {
    let [x, y] = aperture_sun(LensMode::FastCircular, "fast-sun")?;
    assert!(x.min(y) > 100 && x.abs_diff(y) < x / 12);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件，核对相机光圈对太阳盘的卷积"]
fn hardware_pt_aperture_camera_sun() -> Result<(), Box<dyn std::error::Error>> {
    let radii = aperture_sun(fft_bloom::Aperture::Camera.into(), "camera-sun")?;
    assert!(radii.into_iter().all(|r| r > 5));
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件，核对导入瞳孔对太阳盘的卷积"]
fn hardware_pt_aperture_pupil_sun() -> Result<(), Box<dyn std::error::Error>> {
    let radii = aperture_sun(fft_bloom::Aperture::Pupil.into(), "pupil-sun")?;
    assert!(radii.into_iter().all(|r| r > 5));
    Ok(())
}

fn aperture_sun(mode: LensMode, stem: &str) -> Result<[usize; 2], Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    gpu.set_lens_bloom(Some(mode));
    let [width, height] = [1916usize, 997];
    let [cx, cy] = [940usize, 495];
    let target = diffraction_target(&host, gpu, [width, height])?;
    // 游戏中大气外太阳法向辐照度 Y=8、角半径约 0.00465 rad。此视场中盘半径为 5 像素。
    let sky = [0.19, 0.31, 0.51, 1.];
    let sun = [120_000., 110_000., 95_000., 1.];
    let mut pixels = vec![sky; width * height];
    let mut disc = Vec::new();
    for dy in -5i32..=5 {
        for dx in -5i32..=5 {
            if dx * dx + dy * dy <= 25 {
                pixels[(cy as i32 + dy) as usize * width + (cx as i32 + dx) as usize] = sun;
                disc.push((dx, dy));
            }
        }
    }
    let (hdr, display) = present_diffraction(gpu, target, &pixels)?;
    // 用同一天空对比度量可见半径，覆盖有限太阳盘对细线的展宽作用。
    let [horizontal, vertical] =
        [(1, 0, width - 1 - cx), (0, 1, height - 1 - cy)].map(|(dx, dy, end)| {
            (0..=end)
                .rfind(|&r| {
                    let i = (cy + dy * r) * width + cx + dx * r;
                    let pixel: [f32; 4] = bytemuck::cast(hdr.as_chunks::<16>().0[i]);
                    pixel[1] > 1.01 * sky[1]
                })
                .unwrap_or(0)
        });
    eprintln!("{mode:?} 绿通道比天空亮 1% 的半径，横向 {horizontal} px，纵向 {vertical} px");
    if let LensMode::Diffraction(shape) = mode {
        let kernel = shape.kernel()?;
        let side = kernel.extent()[0] as usize;
        let radius = (side / 2) as i32;
        for (dx, dy) in [
            (0, 249),
            (0, 256),
            (0, 264),
            (0, 400),
            (320, 0),
            (690, 0),
            (940, 0),
        ] {
            let x = (cx as i32 + dx) as usize;
            let y = (cy as i32 + dy) as usize;
            let actual: [f32; 4] = bytemuck::cast(hdr.as_chunks::<16>().0[y * width + x]);
            for (c, weights) in kernel.channels().iter().enumerate() {
                // 画外补黑。只积分能落在本帧的背景，再逐个加入太阳盘相对背景的额外辐亮度。
                let x0 = (x + radius as usize + 1).saturating_sub(width);
                let x1 = (x + radius as usize + 1).min(side);
                let y0 = (y + radius as usize + 1).saturating_sub(height);
                let y1 = (y + radius as usize + 1).min(side);
                let mass: f64 = weights
                    .chunks_exact(side)
                    .take(y1)
                    .skip(y0)
                    .flat_map(|row| &row[x0..x1])
                    .copied()
                    .map(f64::from)
                    .sum();
                let flux: f64 = disc
                    .iter()
                    .map(|(sx, sy)| {
                        let (kx, ky) = (dx - sx, dy - sy);
                        if kx.abs() <= radius && ky.abs() <= radius {
                            f64::from(
                                weights[(radius + ky) as usize * side + (radius + kx) as usize],
                            )
                        } else {
                            0.0
                        }
                    })
                    .sum();
                let reference = mass * f64::from(sky[c]) + flux * f64::from(sun[c] - sky[c]);
                assert!(
                    (f64::from(actual[c]) - reference).abs() < 3e-4 + 0.002 * reference,
                    "太阳翼部 {dx},{dy} 通道 {c}: {} / {reference}",
                    actual[c]
                );
            }
        }
    } else {
        for r in [64usize, 128, 256] {
            let spread = 0.545 * 4. / 5. / (std::f64::consts::PI * r as f64).powi(3);
            let reference = disc.len() as f64 * f64::from(sun[1] - sky[1]) * spread;
            for (x, y) in [(cx + r, cy), (cx, cy + r)] {
                let pixel: [f32; 4] = bytemuck::cast(hdr.as_chunks::<16>().0[y * width + x]);
                assert!(
                    (f64::from(pixel[1] - sky[1]) / reference - 1.).abs() < 0.30,
                    "圆孔太阳远翼 {r} px 的绿通道增量 {}，参考 {reference}",
                    pixel[1] - sky[1]
                );
            }
        }
    }
    if let Some(path) = std::env::var_os("PRIME_PT_DIFFRACTION_OUTPUT") {
        let path = std::path::Path::new(&path);
        std::fs::create_dir_all(path)?;
        write_diffraction_png(
            &path.join(format!("{stem}-display.png")),
            &display,
            width,
            height,
        )?;
        std::fs::write(path.join(format!("{stem}-linear.rgba32f")), hdr)?;
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok([horizontal, vertical])
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证自动测光只改变宿主呈现、不改变HDR"]
fn hardware_pt_auto_exposure_preserves_hdr() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    let options = Options::new(false, streamline::Quality::Dlaa, pt::REALTIME_SAMPLES, 6)?;
    let environment = Environment::default();
    host.render_options(&mut gpu, &image, &camera(), environment, options)?;
    let manual = image.download(&mut gpu.context)?;
    let manual_hdr = exposure_hdr(&mut gpu)?;
    gpu.invalidate()?;
    gpu.test_restart_sample_sequence(0)?;
    gpu.display_timeline.set_enabled(true);
    host.render_options(&mut gpu, &image, &camera(), environment, options)?;
    let automatic = image.download(&mut gpu.context)?;
    let state = exposure_state(&mut gpu)?;
    assert_eq!(
        manual_hdr,
        exposure_hdr(&mut gpu)?,
        "测光不能改写同一绝对样本的HDR"
    );
    assert_eq!(state[1], 1);
    assert!(f32::from_bits(state[0]).is_finite() && f32::from_bits(state[0]).abs() > 0.01);
    assert_ne!(manual, automatic, "自动曝光必须实际影响宿主呈现");
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证强度缩放测光目标并保持适应历史"]
fn hardware_pt_auto_exposure_strength_scales_target_ev() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    freeze_exposure_clock(&mut gpu);
    gpu.set_display_options(1.0, 0.05, 0.75, 0.4)?;
    let options = Options::new(false, streamline::Quality::Dlaa, pt::REALTIME_SAMPLES, 6)?;
    let first_sample = gpu.absolute_sample;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let full = exposure_state(&mut gpu)?;
    gpu.set_display_options(0.5, 0.05, 0.75, 0.4)?;
    gpu.invalidate()?;
    gpu.test_restart_sample_sequence(first_sample)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let half = exposure_state(&mut gpu)?;
    assert!(f32::from_bits(full[2]).abs() > 0.01);
    assert!((f32::from_bits(half[2]) - 0.5 * f32::from_bits(full[2])).abs() < 1e-6);
    assert_eq!(full[0], half[0], "零帧间隔保留已适应的曝光");
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证显示曲线只改变呈现"]
fn hardware_pt_drt_controls_only_change_display() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    gpu.set_display_options(0.0, 0.05, 0.75, 0.4)?;
    let options = Options::new(false, streamline::Quality::Dlaa, pt::REALTIME_SAMPLES, 6)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let first = image.download(&mut gpu.context)?;
    let hdr = exposure_hdr(&mut gpu)?;
    gpu.set_display_options(0.0, 0.5, 0.0, 0.95)?;
    gpu.invalidate()?;
    gpu.test_restart_sample_sequence(0)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    assert_eq!(hdr, exposure_hdr(&mut gpu)?);
    assert_ne!(first, image.download(&mut gpu.context)?);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证resize保留曝光适应历史"]
fn hardware_pt_auto_exposure_resize_preserves_adaptation() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    gpu.display_timeline.set_enabled(true);
    freeze_exposure_clock(&mut gpu);
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 6)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let before = exposure_state(&mut gpu)?;
    let resized = Image::new(&mut gpu.context, [64, 32, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(resized.into());
    let dark = Environment {
        sun_direction: [0.0; 3],
        ..Environment::default()
    };
    host.render_options(&mut gpu, &resized, &camera(), dark, options)?;
    let after = exposure_state(&mut gpu)?;
    assert_eq!(after[0], before[0], "零适应间隔下resize不能跳到新场景曝光");
    assert_ne!(after[2], before[2], "全黑场景应建立不同的测光目标");
    assert_eq!(after[1], 1);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证自动曝光下手动EV只作用于最终呈现"]
fn hardware_pt_auto_exposure_manual_ev_is_display_only() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    gpu.display_timeline.set_enabled(true);
    freeze_exposure_clock(&mut gpu);
    let options = Options::new(false, streamline::Quality::Dlaa, pt::REALTIME_SAMPLES, 6)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let initial = image.download(&mut gpu.context)?;
    let initial_hdr = exposure_hdr(&mut gpu)?;
    let initial_state = exposure_state(&mut gpu)?;
    gpu.invalidate()?;
    gpu.test_restart_sample_sequence(0)?;
    let environment = Environment {
        exposure_ev: 2.0,
        ..Environment::default()
    };
    host.render_options(&mut gpu, &image, &camera(), environment, options)?;
    let exposed = image.download(&mut gpu.context)?;
    assert_eq!(initial_hdr, exposure_hdr(&mut gpu)?);
    assert_eq!(
        initial_state,
        exposure_state(&mut gpu)?,
        "手动EV不能进入测光输入"
    );
    let brightness = |pixels: &[u8]| {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[..3].iter().map(|&v| u64::from(v)).sum::<u64>())
            .sum::<u64>()
    };
    assert!(brightness(&exposed) > brightness(&initial));
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证无天空维度完全旁路天空与 Aerial"]
fn hardware_pt_disabled_sky() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let environment = Environment {
        sun_direction: [0.; 3],
        ..Default::default()
    };
    let empty = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 100., [0.5; 2], [1.; 3], 1000.);
    gpu.set_scene(&mesh)?;
    let surface = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    for pixels in [empty, surface] {
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [0, 0, 0, 255])
        );
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = (bits >> 10) & 31;
    let fraction = u32::from(bits & 1023);
    if exponent == 0 {
        sign * fraction as f32 * 2.0_f32.powi(-24)
    } else if exponent == 31 {
        f32::from_bits((u32::from(bits & 0x8000) << 16) | 0x7f80_0000 | (fraction << 13))
    } else {
        f32::from_bits(
            (u32::from(bits & 0x8000) << 16)
                | ((u32::from(exponent) + 112) << 23)
                | (fraction << 13),
        )
    }
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；任意非共面 quad 与两退化 quad 必须等价"]
fn hardware_pt_general_quad_matches_two_triangles() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    gpu.set_atlas(
        2,
        2,
        &[
            255, 80, 40, 255, 20, 200, 100, 255, 50, 80, 240, 255, 240, 220, 90, 127,
        ],
    )?;
    let p = [
        [-1.4, -1.2, 2.0],
        [1.1, -0.8, 2.6],
        [0.8, 1.4, 2.15],
        [-0.7, 0.9, 1.7],
    ];
    let uv = [[0., 0.], [1., 0.2], [0.8, 1.], [0.1, 0.9]];
    let tint = [0.8, 0.7, 0.6];
    let mut mesh = Mesh {
        quads: vec![Quad::new(p, uv, tint, Material::Opaque)],
    };
    gpu.set_scene(&mesh)?;
    let first_sample = gpu.absolute_sample;
    let whole = host.render(&mut gpu, &image, &camera())?;
    mesh.quads = [[0, 1, 2, 2], [0, 2, 3, 3]]
        .map(|corners| {
            Quad::new(
                corners.map(|i| p[i]),
                corners.map(|i| uv[i]),
                tint,
                Material::Opaque,
            )
        })
        .into();
    gpu.set_scene(&mesh)?;
    gpu.test_restart_sample_sequence(first_sample)?;
    let split = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(
        whole, split,
        "quad 的两半身份、真实四角、UV 与覆盖率不能受表示方式影响"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；quad 材质字段必须实际影响不透明表面"]
fn hardware_pt_quad_material_preset_is_consumed() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = image(&mut gpu)?;
    let p = [[-2., -2., 2.], [2., -2., 2.], [2., 2., 2.], [-2., 2., 2.]];
    let mut pixels = Vec::new();
    for material in [Material::Opaque, Material::Smooth] {
        gpu.set_scene(&Mesh {
            quads: vec![Quad::new(p, [[0.5; 2]; 4], [0.8; 3], material)],
        })?;
        pixels.push(host.render(&mut gpu, &image, &camera())?);
    }
    assert_ne!(
        pixels[0], pixels[1],
        "roughness 预设不能成为仅用于填充的字段"
    );
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn float_pixel(bytes: &[u8], extent: [u32; 2], components: usize) -> Vec<f32> {
    let texel = (extent[1] / 2 * extent[0] + extent[0] / 2) as usize;
    let width = bytes.len() / ((extent[0] * extent[1]) as usize * components);
    (0..components)
        .map(|c| float_component(bytes, texel * components + c, width))
        .collect()
}

fn float_component(bytes: &[u8], component: usize, width: usize) -> f32 {
    let start = component * width;
    match width {
        2 => half(u16::from_le_bytes(
            bytes[start..start + 2].try_into().unwrap(),
        )),
        4 => f32::from_le_bytes(bytes[start..start + 4].try_into().unwrap()),
        _ => panic!("测试纹理分量只支持 FP16 或 FP32"),
    }
}

fn finite_half_image(bytes: &[u8]) -> bool {
    bytes.as_chunks::<2>().0.iter().all(|word| {
        let bits = u16::from_le_bytes(*word);
        bits & 0x7c00 != 0x7c00
    })
}

#[test]
#[ignore = "独立进程：真实RR跨anchor与局部更新保留历史；不与其它SDK测试同进程运行"]
fn hardware_pt_rr_rebases_history_across_local_changes() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let image = Image::new(&mut gpu.context, [512, 512, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(image.into());
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 3.0, [0.5; 2], [0.8; 3], 10.0);
    for quad in &mut mesh.quads {
        quad.translate([64.0, 0.0, 0.0]);
    }
    gpu.set_scene(&mesh)?;
    let mut camera = camera();
    camera.origin[0] = 63.75;
    let initial = Options::new(true, streamline::Quality::Dlaa, 1, 6)?;
    host.render_options(&mut gpu, &image, &camera, Environment::default(), initial)?;
    assert!(gpu.rr_previous.is_some(), "首帧必须实际完成RR求值");

    // 只调整本地几何表示与属性；场景实际位置保持不变，前后相机跨64m网格。
    for quad in &mut mesh.quads {
        quad.translate([-64.0, 0.0, 0.0]);
        quad.set_tint([0.7; 3]);
    }
    gpu.set_scene(&mesh)?;
    gpu.anchor = [64.0, 0.0, 0.0];
    camera.origin[0] = 0.25;
    camera.right[0] *= 1.2;
    camera.up[1] *= 1.2;
    gpu.settings.seed += 1;
    let options = Options::new(true, streamline::Quality::Dlaa, 1, 8)?;
    let environment = Environment {
        sun_direction: [0.6, 0.8, 0.0],
        ..Environment::default()
    };
    let counts = Buffer::new(&mut gpu.context, 512 * 512 * 4, true)?;
    gpu.view.as_mut().unwrap().counts = counts;
    gpu.renderer = None;
    host.render_options(&mut gpu, &image, &camera, environment, options)?;
    assert!(gpu.rr_previous.is_some());
    let view = gpu.view.as_ref().unwrap();
    assert_eq!(view.progress.samples(), 2, "RR仍只累计本帧样本");
    let motion = float_pixel(
        &view.rr.images[2].download(&mut gpu.context)?,
        [512, 512],
        2,
    );
    assert!(
        motion[0] > 20.0 && motion[0] < 100.0,
        "局部更新/FOV/太阳/预算/seed不能抹除运动，anchor不能引入64m伪运动：{motion:?}"
    );
    let mut sample_counts = vec![0; counts.len() as usize];
    counts.read(&mut gpu.context, 0, &mut sample_counts)?;
    assert!(
        sample_counts
            .as_chunks::<4>()
            .0
            .iter()
            .all(|word| u32::from_le_bytes(*word) == 2)
    );

    for event in [GlobalReset::WorldReplaced, GlobalReset::SceneDomainReplaced] {
        gpu.invalidate_history(event)?;
        camera.origin[0] += 0.25;
        host.render_options(&mut gpu, &image, &camera, environment, options)?;
        let motion = float_pixel(
            &gpu.view.as_ref().unwrap().rr.images[2].download(&mut gpu.context)?,
            [512, 512],
            2,
        );
        assert!(
            motion.iter().all(|value| value.abs() < 0.001),
            "封闭全局事件必须重置guide：{motion:?}"
        );
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需执行 labPbrFixture，使用实际 SPBR 贴图和 Vulkan 读回首表面"]
fn hardware_spbr_surface() -> Result<(), Box<dyn std::error::Error>> {
    let packet = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/lab-pbr-fixture.bin"
    ))?;
    let mut catalog = prime_minecraft::Catalog::default();
    let mut candidate = catalog.stage_resources(&packet)?;
    let albedo = [255; 80 * 16 * 4];
    let maps = candidate.material_resources([80, 16], &albedo)?;
    let host = Host::create(false, true)?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    // 铜块首纹素的切线法线倾斜超过 45°，从斜上方观察使反射光线能射向天空。
    let camera = Camera {
        origin: [0., 2.25, 0., 0.],
        forward: [0., -0.6, 0.8, 0.],
        up: [0., 0.8, 0.6, 0.],
        ..camera()
    };
    let extent = [33, 33];
    let output_image = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output_image.into());
    let definitions = [0u32, 1].map(|tile| {
        let mut material = MaterialDefinition::default();
        material.control[2] = MaterialDefinition::MERS
            | MaterialDefinition::AUXILIARY
            | MaterialDefinition::LAB_SPECULAR;
        material.control[3] = MaterialDefinition::AUTHORED_NORMAL;
        material.uv_bounds = [tile as f32 / 5., 0., (tile + 1) as f32 / 5., 1.];
        material
    });
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: maps.extent,
        albedo: &albedo,
        materials: &definitions,
        mers: &maps.mers,
        auxiliary: &maps.auxiliary,
        emission_alias_words: &[],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]; 2],
    })?;
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd();
    for (tile, expected_roughness, class) in [(0, 35. / 255., 1), (1, 1. / 255., 0)] {
        let x = tile as f32 / 5.;
        // 使用每张源图第一个 texel 的内部区域，同时保留二维 UV 导数。
        let mesh = Mesh {
            quads: vec![Quad::with_material(
                [[-8., -8., 3.], [8., -8., 3.], [8., 8., 3.], [-8., 8., 3.]],
                [
                    [x + 0.002, 0.01],
                    [x + 0.008, 0.01],
                    [x + 0.008, 0.05],
                    [x + 0.002, 0.05],
                ],
                [1.; 3],
                prime_scene::MaterialId::new(tile),
            )],
        };
        gpu.set_scene(&mesh)?;
        host.render_options(gpu, &output_image, &camera, Environment::default(), options)?;
        let guides = gpu.view.as_ref().unwrap().rr.images;
        let n = float_pixel(&guides[3].download(&mut gpu.context)?, extent, 4);
        let specular = float_pixel(&guides[5].download(&mut gpu.context)?, extent, 4);
        let motion = float_pixel(&guides[0].download(&mut gpu.context)?, extent, 4);
        let material_class = motion[3] as u32 & 3;
        eprintln!(
            "SPBR tile={tile} normal_roughness={n:?} specular={specular:?} class={material_class}"
        );
        assert!((n[3] - expected_roughness).abs() < 0.001 && n[0].abs() + n[1].abs() > 0.4);
        assert_eq!(material_class, class);
        assert!((specular[3] - expected_roughness).abs() < 0.001);
        // 白底纯金属返回单位能量。钻石的中心像素按贴图法线、视角和 F0 求菲涅尔反射。
        let reflectance = if tile == 0 {
            1.
        } else {
            let amplitude = (44.0_f32 / 255.).sqrt();
            let ior = (1. + amplitude) / (1. - amplitude);
            let incident_cos = (n[1] * camera.forward[1] + n[2] * camera.forward[2]).abs();
            let transmitted_cos = (1. - (1. - incident_cos.powi(2)) / ior.powi(2)).sqrt();
            let rs =
                (incident_cos - ior * transmitted_cos) / (incident_cos + ior * transmitted_cos);
            let rp =
                (ior * incident_cos - transmitted_cos) / (ior * incident_cos + transmitted_cos);
            0.5 * (rs.powi(2) + rp.powi(2))
        };
        assert!(
            specular[..3]
                .iter()
                .all(|f| (f - reflectance).abs() < 0.002)
        );
        if tile == 0 {
            let nrd = gpu.view.as_ref().unwrap().nrd.as_ref().unwrap();
            let [specular, direction, filtered] = nrd.specular_signals();
            let diffuse = nrd.diffuse_pair()[0];
            let mut pixel = |image: Image| -> Result<Vec<f32>, Error> {
                Ok(float_pixel(&image.download(&mut gpu.context)?, extent, 4))
            };
            let input = pixel(specular)?[0];
            let output = pixel(filtered)?[0];
            eprintln!("SPBR 铜块高光 SH0 输入={input} 输出={output}");
            assert_eq!(pixel(diffuse)?[0], 0., "纯金属没有漫反射");
            assert!(input > 0. && output > 0.);
            assert!(pixel(direction)?[..3].iter().any(|x| x.abs() > 0.001));
            // 沿纹理斜率反向掠视铜块，原始 Ns 将理想反射指入实心几何。
            let ray_x = 0.999_f32.sqrt();
            let ray_z = 0.001_f32.sqrt();
            let grazing = Camera {
                origin: [-3. * ray_x, 0., 3. - 3. * ray_z, 0.],
                forward: [ray_x, 0., ray_z, 0.],
                right: [ray_z, 0., -ray_x, 0.],
                up: [0., 1., 0., 0.],
            };
            host.render_options(
                gpu,
                &output_image,
                &grazing,
                Environment::default(),
                options,
            )?;
            let corrected = float_pixel(&guides[3].download(&mut gpu.context)?, extent, 4);
            let nv = -corrected[0] * ray_x - corrected[2] * ray_z;
            let reflected_cos = -2. * nv * corrected[2] - ray_z;
            assert!(
                nv > 0. && (reflected_cos - 0.01).abs() < 0.001,
                "修正后的掠射余弦 {reflected_cos}，法线 {corrected:?}"
            );
        }
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan/SPIR-V NRD 输入、历史、移动和切换"]
fn hardware_nrd_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, std::env::var_os("PRIME_NRD_CALLABLE").is_none())?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let extent = if std::env::var_os("PRIME_NRD_4K").is_some() {
        [3840, 2160]
    } else {
        [128, 96]
    };
    let image = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(image.into());
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 3., [0.5; 2], [0.7, 0.4, 0.2], 8.);
    gpu.set_scene(&mesh)?;
    let env = Environment {
        sun_direction: [0.28f32.sqrt(), 0.6, -0.6],
        ..Default::default()
    };
    let raw = Options::new(false, streamline::Quality::Dlaa, 2, 4)?;
    let queries = [
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
    ];
    gpu.test_roots
        .extend(queries.map(vulkan::native::Resource::from));
    let mut camera = camera();
    let frames = if extent[0] == 3840 { 14 } else { 68 };
    let mut durations = vec![];
    let mut diffuse = Vec::new();
    for i in 0..frames {
        if i == frames - 1 {
            camera.origin[0] = 0.125;
            camera.origin[2] = 0.25;
        }
        let [_, ns] = host.render_profiled(
            &mut gpu,
            &image,
            &camera,
            env,
            raw.with_nrd(),
            Some(queries),
        )?;
        if i >= 3 && i + 1 < frames {
            durations.push(ns as f64 * 1e-6);
        }
        if extent[0] == 128 && i >= 20 && i + 1 < frames {
            let pair = gpu
                .view
                .as_ref()
                .unwrap()
                .nrd
                .as_ref()
                .unwrap()
                .diffuse_pair();
            let mut values = [0.; 2];
            for (value, image) in values.iter_mut().zip(pair) {
                *value = f64::from(float_pixel(&image.download(&mut gpu.context)?, extent, 4)[0]);
            }
            diffuse.push(values);
        }
    }
    let view = gpu.view.as_ref().unwrap();
    assert!(view.nrd.is_some() && gpu.rr_previous.is_some());
    let motion = float_pixel(&view.rr.images[6].download(&mut gpu.context)?, extent, 4);
    assert!(
        motion[0] > 0.5 && motion[1].abs() < 0.1 && (motion[2] - 0.25).abs() < 0.002,
        "横移并向前移动的 2.5D 运动 {motion:?}"
    );
    let center = float_pixel(&view.rr.images[4].download(&mut gpu.context)?, extent, 4);
    assert!(center[0] > 0. && center[3] == 1., "首表面 {center:?}");
    let hdr = view.presentation.linear_hdr.download(&mut gpu.context)?;
    assert!(
        hdr.chunks_exact(4)
            .all(|p| f32::from_le_bytes(p.try_into().unwrap()).is_finite())
    );
    let at = ((extent[1] / 2 * extent[0] + extent[0] / 2) * 16) as usize;
    assert!(f32::from_le_bytes(hdr[at..at + 4].try_into()?) > 0.);
    durations.sort_by(f64::total_cmp);
    eprintln!(
        "NRD native={}x{} frames={} frame_gpu_ms_median={:.3}",
        extent[0],
        extent[1],
        frames,
        durations[durations.len() / 2]
    );
    if !diffuse.is_empty() {
        let variance = |channel: usize| {
            let n = diffuse.len() as f64;
            let mean = diffuse.iter().map(|v| v[channel]).sum::<f64>() / n;
            diffuse
                .iter()
                .map(|v| (v[channel] - mean).powi(2))
                .sum::<f64>()
                / n
        };
        let [raw, filtered] = [variance(0), variance(1)];
        eprintln!(
            "NRD static diffuse frames={} raw_variance={raw:.8} filtered_variance={filtered:.8}",
            diffuse.len()
        );
        assert!(raw > 0. && filtered < raw, "静止材质漫反射的帧间波动应下降");
    }
    host.render_options(&mut gpu, &image, &camera, env, raw)?;
    assert!(gpu.view.as_ref().unwrap().nrd.is_none());
    if extent[0] == 3840 {
        let mut timings = Vec::new();
        for i in 0..frames {
            let [_, ns] =
                host.render_profiled(&mut gpu, &image, &camera, env, raw, Some(queries))?;
            if i >= 3 && i + 1 < frames {
                timings.push(ns as f64 * 1e-6);
            }
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "PT native=3840x2160 denoiser=off frame_gpu_ms_median={:.3}",
            timings[timings.len() / 2]
        );
    }
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[path = "gpu_nrd_tests.rs"]
mod nrd_surfaces;

#[test]
#[ignore = "实际太阳遮挡距离与 SIGMA 历史输出"]
fn hardware_nrd_sun_blocker() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::create(false, true)?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 3., [0.5; 2], [1.; 3], 8.);
    let env = Environment {
        sun_direction: [0.8, 0., -0.6],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 2, 4)?.with_nrd();
    let mut read_shadow = |mesh: &Mesh| -> Result<[f32; 2], Box<dyn std::error::Error>> {
        gpu.set_scene(mesh)?;
        for _ in 0..12 {
            host.render_options(gpu, &output, &camera(), env, options)?;
        }
        let images = gpu
            .view
            .as_ref()
            .unwrap()
            .nrd
            .as_ref()
            .unwrap()
            .shadow_pair();
        let mut samples = [0.; 2];
        for (sample, img) in samples.iter_mut().zip(images) {
            let bytes = img.download(&mut gpu.context)?;
            let index = (16 * 32 + 16) * 4;
            *sample = f32::from_le_bytes(bytes[index..index + 4].try_into()?);
        }
        Ok(samples)
    };
    let lit = read_shadow(&mesh)?;
    mesh.quads.push(Quad::new(
        [
            [1.3, -0.75, 1.5],
            [2.7, -0.75, 1.5],
            [2.7, 0.75, 1.5],
            [1.3, 0.75, 1.5],
        ],
        [[0.5; 2]; 4],
        [1.; 3],
        Material::Opaque,
    ));
    let blocked = read_shadow(&mesh)?;
    eprintln!("SIGMA [penumbra,shadow] lit={lit:?} blocked={blocked:?}");
    assert!(lit[0] >= 65500. && lit[1] > 0.95);
    assert!(blocked[0] > 0. && blocked[0] < 0.08 && blocked[1] < 0.05);
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独立进程运行此 filter：early interposer + PT/guide/RR/超分/呈现完整链路"]
fn hardware_pt_rr_full_chain() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr()?;
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    let extent = [1920, 1080];
    let image = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(image.into());
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 3.0, [0.5; 2], [0.8, 0.5, 0.2], 10.0);
    // 相机后的局部表面只被反射辅助射线命中；不会遮住太阳或主视图。
    add_quad(&mut mesh, -2.0, [0.5; 2], [1.0; 3], 0.25);
    gpu.set_scene(&mesh)?;
    let environment = Environment {
        sun_direction: [0.28f32.sqrt(), 0.6, -0.6],
        ..Default::default()
    };
    let mut camera = camera();
    for frame in 0..4 {
        if frame == 1 {
            camera.origin[0] = 0.125;
        }
        if frame == 3 {
            gpu.set_scene(&mesh)?;
        }
        host.render_rr(&mut gpu, &image, &camera, environment)?;
        let view = gpu.view.as_ref().ok_or("RR 视图未创建")?;
        assert!(view.rr_enabled);
        assert_eq!(view.progress.samples(), 2, "RR 输入每帧独立双样本");
        assert!(
            gpu.rr_previous.is_some(),
            "RR 必须成功求值，不能只走 noisy 回退"
        );
        let images = &view.rr.images;
        let noisy = images[0].download(&mut gpu.context)?;
        let output = images[6].download(&mut gpu.context)?;
        assert!(finite_half_image(&noisy) && finite_half_image(&output));
        let noisy_pixel = float_pixel(&noisy, extent, 4);
        let output_pixel = float_pixel(&output, extent, 4);
        assert!(
            noisy_pixel[..3].iter().any(|&v| v > 0.01),
            "太阳和大气 HDR：{noisy_pixel:?}"
        );
        assert!(
            output_pixel[..3].iter().any(|&v| v > 0.01),
            "RR 输出：{output_pixel:?}"
        );
        let depth = images[1].download(&mut gpu.context)?;
        let center = ((extent[1] / 2 * extent[0] + extent[0] / 2) as usize) * 4;
        let depth = f32::from_le_bytes(depth[center..center + 4].try_into()?);
        assert!((depth - 3.0).abs() < 1e-4, "首命中 view-Z：{depth}");
        let motion = float_pixel(&images[2].download(&mut gpu.context)?, extent, 2);
        let expected = if frame == 1 { 40.0 } else { 0.0 };
        assert!(
            (motion[0] - expected).abs() < 0.1 && motion[1].abs() < 0.1,
            "frame={frame}，运动={motion:?}，预期x={expected}"
        );
        let normal = float_pixel(&images[3].download(&mut gpu.context)?, extent, 4);
        assert!(normal[0].abs() < 1e-4 && normal[1].abs() < 1e-4 && (normal[2] + 1.0).abs() < 1e-4);
        assert!((normal[3] - 0.9).abs() < 0.001);
        let diffuse = float_pixel(&images[4].download(&mut gpu.context)?, extent, 4);
        let specular = float_pixel(&images[5].download(&mut gpu.context)?, extent, 4);
        assert!(diffuse[..3].iter().all(|&v| v > 0.0 && v < 1.0));
        assert!(specular[..3].iter().all(|&v| v > 0.0 && v < 1.0));
        let distance = images[7].download(&mut gpu.context)?;
        let distance = f32::from_le_bytes(distance[center..center + 4].try_into()?);
        assert!((distance - 5.0).abs() < 0.001, "反射段距离：{distance}");
        let reflected = float_pixel(&images[8].download(&mut gpu.context)?, extent, 2);
        let expected = if frame == 1 { 15.0 } else { 0.0 };
        assert!(
            (reflected[0] - expected).abs() < 0.1 && reflected[1].abs() < 0.1,
            "frame={frame}，虚像运动={reflected:?}，预期x={expected}"
        );
        assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    }
    let presented = image.download(&mut gpu.context)?;
    assert!(
        presented
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    // 模拟配置失败后的 SDK 失效；旧视图尺寸相同也须重新配置并实际求值。
    gpu.rr_runtime = None;
    host.render_rr(&mut gpu, &image, &camera, environment)?;
    assert!(gpu.rr_runtime.is_some() && gpu.rr_previous.is_some());
    // 同一个 RR owner 切换尺寸和档位；Performance 的运动仍以输入像素计量。
    let performance = Options::new(true, streamline::Quality::Performance, 3, 6)?;
    camera.origin[0] = 0.0;
    host.render_options(&mut gpu, &image, &camera, environment, performance)?;
    assert_eq!(gpu.view.as_ref().unwrap().extent, [960, 540]);
    assert_eq!(gpu.view.as_ref().unwrap().output_extent, extent);
    assert!(gpu.rr_previous.is_some(), "超分必须实际求值成功");
    let first_noisy = gpu.view.as_ref().unwrap().rr.images[0].download(&mut gpu.context)?;
    // 只改变测试中的存储位置，使真实批次的计数与和可回读。
    let view = gpu.view.as_mut().unwrap();
    view.counts = Buffer::new(&mut gpu.context, 960 * 540 * 4, true)?;
    view.accumulation = Buffer::new(&mut gpu.context, 960 * 540 * 16, true)?;
    gpu.renderer = None;
    camera.origin[0] += 0.125;
    host.render_options(&mut gpu, &image, &camera, environment, performance)?;
    let view = gpu.view.as_ref().unwrap();
    assert_eq!(view.progress.samples(), 2);
    let input = view.extent;
    for (index, image) in view.rr.images.iter().enumerate() {
        let expected = if index == 6 { extent } else { input };
        assert_eq!(image.extent(), [expected[0], expected[1], 1]);
    }
    let motion = float_pixel(&view.rr.images[2].download(&mut gpu.context)?, input, 2);
    let specular = float_pixel(&view.rr.images[8].download(&mut gpu.context)?, input, 2);
    assert!(
        (motion[0] - 20.0).abs() < 0.1 && motion[1].abs() < 0.1,
        "超分运动：{motion:?}"
    );
    assert!(
        (specular[0] - 7.5).abs() < 0.1 && specular[1].abs() < 0.1,
        "超分虚像运动：{specular:?}"
    );
    let noisy = view.rr.images[0].download(&mut gpu.context)?;
    assert_ne!(first_noisy, noisy);
    let mut counts = vec![0; view.counts.len() as usize];
    view.counts.read(&mut gpu.context, 0, &mut counts)?;
    assert!(
        counts
            .as_chunks::<4>()
            .0
            .iter()
            .all(|word| u32::from_le_bytes(*word) == 2)
    );
    let mut accumulation = vec![0; view.accumulation.len() as usize];
    view.accumulation
        .read(&mut gpu.context, 0, &mut accumulation)?;
    let mean = float_pixel(&noisy, input, 4);
    let pixel = ((input[1] / 2 * input[0] + input[0] / 2) as usize) * 16;
    for (channel, mean) in mean[..3].iter().copied().enumerate() {
        let offset = pixel + channel * 4;
        let sum = f32::from_le_bytes(accumulation[offset..offset + 4].try_into()?);
        assert!((mean - sum / 2.0).abs() <= mean.abs().max(1.0) * 0.001);
    }
    let sample = gpu.absolute_sample;
    host.render_options(&mut gpu, &image, &camera, environment, performance)?;
    assert_eq!(gpu.absolute_sample, sample + 2);
    let next_noisy = gpu.view.as_ref().unwrap().rr.images[0].download(&mut gpu.context)?;
    assert_ne!(noisy, next_noisy, "静止相机的逐帧随机样本应独立");
    let presented = image.download(&mut gpu.context)?;
    assert!(
        presented
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    let output = gpu.view.as_ref().unwrap().rr.images[6].download(&mut gpu.context)?;
    assert!(finite_half_image(&output));
    // reset 只影响重建历史；SDK 输入尺寸随档位和最终输出改变。
    for (quality, expected) in [
        (streamline::Quality::Quality, [1280, 720]),
        (streamline::Quality::Dlaa, extent),
        (streamline::Quality::Performance, [960, 540]),
    ] {
        host.render_options(
            &mut gpu,
            &image,
            &camera,
            environment,
            Options::new(true, quality, 1, 6)?,
        )?;
        let view = gpu.view.as_ref().unwrap();
        assert_eq!(view.extent, expected);
        let motion = float_pixel(&view.rr.images[2].download(&mut gpu.context)?, expected, 2);
        assert!(
            motion.iter().all(|value| value.abs() < 0.001),
            "档位切换须重置历史：{motion:?}"
        );
        assert!(gpu.rr_previous.is_some());
    }
    let resized = Image::new(&mut gpu.context, [1280, 720, 1], ImageFormat::Rgba8Unorm)?;
    gpu.test_roots.push(resized.into());
    host.render_options(&mut gpu, &resized, &camera, environment, performance)?;
    let view = gpu.view.as_ref().unwrap();
    assert_eq!(view.extent, [640, 360]);
    assert_eq!(view.output_extent, [1280, 720]);
    let motion = float_pixel(
        &view.rr.images[2].download(&mut gpu.context)?,
        view.extent,
        2,
    );
    assert!(
        motion.iter().all(|value| value.abs() < 0.001),
        "resize 须重置历史：{motion:?}"
    );
    assert!(gpu.rr_previous.is_some());
    assert!(
        resized
            .download(&mut gpu.context)?
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    // SDK 求值失败分支独立录制，使用真实输入 HDR 验证整幅超分回退覆盖。
    resized.upload(&mut gpu.context, &vec![0; 1280 * 720 * 4])?;
    view.presentation
        .record(
            &mut gpu.context,
            Binding::StorageImage(resized),
            HdrSource::ScaledRaw,
            gpu.display_timeline.enabled(),
        )?
        .submit_and_wait(&mut gpu.context)?;
    let pixels = resized.download(&mut gpu.context)?;
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[..3].iter().any(|&v| v > 0))
    );
    // 原图像oracle保持自动曝光关闭；另经生产draw验证RR纹理测光与宿主呈现绑定。
    gpu.display_timeline.set_enabled(true);
    host.render_options(&mut gpu, &resized, &camera, environment, performance)?;
    assert!(gpu.rr_previous.is_some(), "自动曝光帧必须真实完成RR求值");
    let automatic = exposure_state(&mut gpu)?;
    assert_eq!(automatic[1], 1);
    assert!(
        [automatic[0], automatic[2], automatic[3]]
            .into_iter()
            .all(|word| f32::from_bits(word).is_finite())
    );
    let mut exposure_frame = [0; 32];
    gpu.view
        .as_ref()
        .unwrap()
        .presentation
        .display
        .parameters()
        .read(&mut gpu.context, 0, &mut exposure_frame)?;
    assert_eq!(u32::from_le_bytes(exposure_frame[..4].try_into()?), 1280);
    assert_eq!(u32::from_le_bytes(exposure_frame[4..8].try_into()?), 720);
    // bridge 只允许一个 RR owner；第二会话必须在任何编码前回退，且保留 PT 累积。
    let mut fallback_session = host.gpu()?;
    let mut fallback = fallback_session.gpu();
    let fallback_image = self::image(&mut fallback)?;
    for expected in [2, 4] {
        host.render_rr(&mut fallback, &fallback_image, &camera, environment)?;
        assert!(fallback.rr_unavailable.is_some());
        let view = fallback.view.as_ref().ok_or("回退 PT 未创建视图")?;
        assert!(!view.rr_enabled);
        assert_eq!(view.progress.samples(), expected);
    }
    let automatic = Options::new(true, streamline::Quality::Dlaa, 2, 4)?.with_nrd_fallback();
    for _ in 0..2 {
        host.render_options(
            &mut fallback,
            &fallback_image,
            &camera,
            environment,
            automatic,
        )?;
        let view = fallback.view.as_ref().unwrap();
        assert!(view.nrd.is_some() && !view.rr_enabled && fallback.rr_previous.is_some());
        assert_eq!(view.extent, view.output_extent);
    }
    let pixels = fallback_image.download(&mut fallback.context)?;
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    fallback_session.finish()?;
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实 Vulkan；屋顶不进入视野，材质覆盖改变相机空气段的太阳遮挡"]
fn terrain_volume_uses_cutout_blockers_outside_the_view() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let output = image(gpu)?;
    exposure_view(gpu)?;
    let ray = Camera {
        right: [0.005, 0., 0., 0.],
        up: [0., 0.005, 0., 0.],
        ..camera()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 1)?;
    let environment = Environment {
        sun_direction: [0., 1., 0.],
        ..Default::default()
    };
    let mesh = Mesh {
        quads: vec![prime_scene::Quad::with_material(
            [
                [-512., 40., -128.],
                [-512., 40., 2048.],
                [512., 40., 2048.],
                [512., 40., -128.],
            ],
            [[0.5; 2]; 4],
            [1.; 3],
            prime_scene::MaterialId::new(0),
        )],
    };
    let cutout = MaterialDefinition::default();
    let render = |gpu: &mut Gpu, alpha: u8| -> Result<f32, Box<dyn std::error::Error>> {
        gpu.replace_resources(ResourceUpload {
            generation: gpu.resource_generation + 1,
            extent: [1, 1],
            albedo: &[255, 255, 255, alpha],
            materials: &[cutout],
            mers: &[0, 255, 242, 0],
            auxiliary: &[128, 128, 0, 85],
            emission_alias_words: &[],
            opacity_quads: &[],
            light_areas: &[[u32::MAX, 0, 0, 0]],
        })?;
        gpu.set_scene(&mesh)?;
        gpu.absolute_sample = 0;
        host.render_options(gpu, &output, &ray, environment, options)?;
        Ok(accumulated_rgb(gpu)?[16 * 32 + 16][1])
    };
    let clear = render(gpu, 0)?;
    let shade = render(gpu, 255)?;
    let restored = render(gpu, 0)?;
    assert!(shade < clear && shade > 0., "屋顶 {shade}，透明 {clear}");
    assert!(
        (restored - clear).abs() < 0.002 * (1. + clear),
        "透明 {clear} -> {restored}"
    );
    eprintln!("地形体积光，透明 {clear}，遮挡 {shade}，恢复 {restored}");
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
