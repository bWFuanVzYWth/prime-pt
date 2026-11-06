//! 独立无窗口设备上的真实 trace/readback；显式运行 ignored 测试。

use super::*;
use crate::scene::{Material, Quad};
use ash::{
    Entry, vk,
    vk::{Handle, TaggedStructure},
};
use simple_engine::{openpbr, shader_build, vulkan};
use std::ffi::CStr;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "scene_bench.rs"]
mod scene_bench;

#[path = "cv_tests.rs"]
mod cv_tests;
#[path = "features_tests.rs"]
mod features_tests;
#[path = "history_tests.rs"]
mod history_tests;
#[path = "render_bench.rs"]
mod render_bench;
#[path = "rendering_tests.rs"]
mod rendering_tests;

struct Host {
    _entry: Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    errors: Box<AtomicUsize>,
    streamline: Option<StreamlineBootstrap>,
    invocation_reorder: bool,
}

struct StreamlineBootstrap;

impl Drop for StreamlineBootstrap {
    fn drop(&mut self) {
        if let Err(error) = crate::streamline::shutdown() {
            eprintln!("{error}");
            std::process::abort();
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
        if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
            (*user.cast::<AtomicUsize>()).fetch_add(1, Ordering::Relaxed);
        }
        eprintln!("validation: {message}");
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
            Some(StreamlineBootstrap)
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
                _entry: entry,
                instance,
                physical: vk::PhysicalDevice::null(),
                device: ash::Device::load_with(|_| std::ptr::null(), vk::Device::null()),
                queue: vk::Queue::null(),
                family: 0,
                debug,
                errors,
                streamline,
                invocation_reorder,
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
                let extensions: Vec<_> = required.iter().map(|name| name.as_ptr()).collect();
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
                    .shader_storage_image_write_without_format(rr);
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
            if self.device.handle() != vk::Device::null() {
                if self.device.device_wait_idle().is_err() {
                    std::process::abort();
                }
                drop(self.streamline.take());
                self.device.destroy_device(None);
            }
            if let Some((extension, messenger)) = &self.debug {
                extension.destroy_debug_utils_messenger(*messenger, None);
            }
            self.instance.destroy_instance(None);
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
    fn gpu(&self) -> Result<Gpu, Error> {
        self.gpu_requested(true)
    }

    fn gpu_requested(&self, requested: bool) -> Result<Gpu, Error> {
        // SAFETY: Host 比会话活得更久；测试同步提交，销毁前没有在途命令。
        let mut gpu = unsafe {
            if self.invocation_reorder {
                Gpu::attach_options(
                    self.instance.handle().as_raw(),
                    self.physical.as_raw(),
                    self.device.handle().as_raw(),
                    self.queue.as_raw(),
                    self.family,
                    0,
                    true,
                    requested,
                    None,
                )?
            } else {
                Gpu::attach(
                    self.instance.handle().as_raw(),
                    self.physical.as_raw(),
                    self.device.handle().as_raw(),
                    self.queue.as_raw(),
                    self.family,
                )?
            }
        };
        gpu.settings.samples = 64;
        gpu.settings.max_bounces = 6;
        gpu.display_timeline.set_enabled(false);
        Ok(gpu)
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
            if self.device.queue_wait_idle(self.queue).is_err() {
                std::process::abort();
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
            if self.device.queue_wait_idle(self.queue).is_err() {
                std::process::abort();
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
    let mut gpu = host.gpu()?;
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
    let mut view = View::new(&mut gpu.context, [32, 32], false)?;
    view.accumulation = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    view.counts = Buffer::new(&mut gpu.context, 32 * 32 * 4, true)?;
    view.prefix = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.view = Some(view);
    // 对照开始前完成 SkyView/Aerial 首次生成，三个受检帧共用已完成的环境资源。
    host.render(&mut gpu, &image, &camera())?;
    let mut reference: Option<Frame> = None;
    let mut unchanged = true;
    for (index, invocation_reorder) in [false, false, true, false].into_iter().enumerate() {
        // 通过生产设置入口切换，只重置计数以比较同一绝对样本序列。
        gpu.set_ser(invocation_reorder)?;
        gpu.invalidate()?;
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
                .all(|word| u32::from_le_bytes(*word) == 64)
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    assert!(unchanged, "程序切换改变了相同绝对样本序列的 HDR 或呈现");
    Ok(())
}

#[test]
#[ignore = "需要 EXT SER；验证运行时开关保留累积样本"]
fn hardware_pt_ser_toggle_preserves_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_ser()?;
    let mut gpu = host.gpu_requested(false)?;
    assert!(!gpu.invocation_reorder);
    let image = image(&mut gpu)?;
    host.render(&mut gpu, &image, &camera())?;
    for (enabled, expected_samples) in [(true, 128), (false, 192), (true, 256)] {
        let started = std::time::Instant::now();
        gpu.set_ser(enabled)?;
        eprintln!("mod SER={enabled} 切换准备耗时={:?}", started.elapsed());
        assert_eq!(gpu.invocation_reorder, enabled);
        assert!(gpu.context.capabilities().invocation_reorder);
        assert_eq!(
            gpu.view.as_ref().unwrap().progress.samples(),
            expected_samples - 64
        );
        host.render(&mut gpu, &image, &camera())?;
        let view = gpu.view.as_ref().unwrap();
        assert_eq!(view.progress.samples(), expected_samples);
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要硬件 RT；验证逻辑设备未启用 SER 时保持普通程序"]
fn hardware_pt_ser_unavailable_uses_plain() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    assert!(!gpu.context.capabilities().invocation_reorder);
    gpu.set_ser(true)?;
    assert!(!gpu.invocation_reorder);
    let image = image(&mut gpu)?;
    host.render(&mut gpu, &image, &camera())?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 64);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证宿主呈现的上下方向"]
fn hardware_pt_host_image_y() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证 alpha cutout 与删除透明表面等价"]
fn hardware_pt_alpha_cutout() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    gpu.set_atlas(2, 1, &[255, 0, 0, 127, 160, 200, 240, 255])?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 2., [0.75, 0.5], [1.; 3], 2.);
    gpu.set_scene(&mesh)?;
    let reference = host.render(&mut gpu, &image, &camera())?;
    add_quad(&mut mesh, 1., [0.25, 0.5], [1.; 3], 1.);
    gpu.set_scene(&mesh)?;
    let cutout = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(center(&reference), center(&cutout));
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证 atlas 与 callable 一起发布及失败保留旧资源"]
fn hardware_pt_material_reload() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [1.; 3], 2.);
    gpu.set_scene(&mesh)?;
    let white = host.render(&mut gpu, &image, &camera())?;
    gpu.set_atlas(1, 1, &[0, 0, 0, 255])?;
    let black = host.render(&mut gpu, &image, &camera())?;
    assert_ne!(center(&white), center(&black));
    assert!(gpu.set_atlas(2, 1, &[255; 4]).is_err());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let unavailable =
        shader_build::RuntimeCompiler::new(root.join("../../target/reload-error"), &[], &[])?;
    let compiler = std::mem::replace(&mut gpu.compiler, unavailable);
    assert!(matches!(
        gpu.set_atlas(1, 1, &[255; 4]),
        Err(Error::MaterialPrograms(openpbr::program::Error::Shader(_)))
    ));
    gpu.compiler = compiler;
    gpu.restart_accumulation()?;
    let retained = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(black, retained);
    gpu.set_atlas(1, 1, &[255; 4])?;
    let restored = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(white, restored);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；失败批次只取消自己且允许下一帧继续"]
fn hardware_pt_failed_recording_preserves_other_pending() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；材质程序不能保留已退役的场景工作集"]
fn hardware_pt_material_program_releases_retired_scene() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证 atlas 与 tint 使用相同线性颜色约定"]
fn hardware_pt_linear_tint() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [1.; 3], 2.);
    gpu.set_atlas(1, 1, &[128, 192, 64, 255])?;
    gpu.set_scene(&mesh)?;
    let texture = host.render(&mut gpu, &image, &camera())?;
    gpu.set_atlas(1, 1, &[255; 4])?;
    for quad in &mut mesh.quads {
        quad.set_tint([128. / 255., 192. / 255., 64. / 255.]);
    }
    gpu.set_scene(&mesh)?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证跨帧合批保持绝对样本身份"]
fn hardware_pt_sample_batching() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [0.5, 1., 0.5], 0.7);
    gpu.set_scene(&mesh)?;
    gpu.settings.samples = 8;
    let batched = host.render(&mut gpu, &image, &camera())?;
    gpu.restart_accumulation()?;
    gpu.settings.samples = 1;
    let mut split = Vec::new();
    for _ in 0..8 {
        split = host.render(&mut gpu, &image, &camera())?;
    }
    assert_eq!(batched, split);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证主查询与阴影候选使用同一 alpha 边界"]
fn hardware_pt_ray_query_alpha() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证相机变化不会保留旧视图的累积"]
fn hardware_pt_camera_resets_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 1., [0.5; 2], [0., 1., 0.], 2.);
    gpu.set_scene(&mesh)?;
    host.render(&mut gpu, &image, &camera())?;
    let mut shifted = camera();
    shifted.origin[0] = 100.;
    let changed = host.render(&mut gpu, &image, &shifted)?;
    gpu.restart_accumulation()?;
    let fresh = host.render(&mut gpu, &image, &shifted)?;
    assert_eq!(changed, fresh);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证太阳 NEE 阴影与 alpha 覆盖一致"]
fn hardware_pt_sun_cutout_shadow() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let environment = Environment {
        sun_direction: [0.28f32.sqrt(), 0.6, -0.6],
        ..Default::default()
    };
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, 3., [0.75, 0.5], [1.; 3], 5.);
    gpu.set_scene(&mesh)?;
    let lit = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let start = mesh.quads.len();
    add_quad(&mut mesh, 2., [0.25, 0.5], [1.; 3], 0.5);
    for quad in &mut mesh.quads[start..] {
        quad.translate([0.95, 1., 0.]);
    }
    gpu.set_scene(&mesh)?;
    gpu.set_atlas(2, 1, &[255, 255, 255, 127, 255, 255, 255, 255])?;
    let cutout = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(center(&lit), center(&cutout));
    gpu.set_atlas(2, 1, &[255; 8])?;
    let shadow = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert!(
        (0..3).all(|i| center(&lit)[i] > center(&shadow)[i] + 8),
        "太阳直照 {:?}，阴影 {:?}",
        center(&lit),
        center(&shadow)
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证天空 azimuth 使用世界太阳方向"]
fn hardware_pt_sky_direction() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut environment = Environment {
        sun_direction: [0., 0.6, 0.8],
        ..Default::default()
    };
    let forward = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    environment.sun_direction = [0., 0.6, -0.8];
    let backward = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 64);
    assert_ne!(center(&forward), center(&backward), "天空应随太阳方向变化");
    environment.sun_direction = [0.8, 0.6, 0.];
    let mut rotated = camera();
    rotated.forward = [1., 0., 0., 0.];
    rotated.right = [0., 0., -1., 0.];
    let rotated = host.render_environment(&mut gpu, &image, &rotated, environment)?;
    assert!(
        forward
            .iter()
            .zip(rotated)
            .all(|(&a, b)| a.abs_diff(b) <= 1)
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证主射线距离驱动 Aerial 散射"]
fn hardware_pt_aerial_distance() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证曝光只改变呈现并保留 PT 和大气历史"]
fn hardware_pt_exposure_preserves_history() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    let mut view = View::new(&mut gpu.context, [32, 32], false)?;
    view.accumulation = Buffer::new(&mut gpu.context, 32 * 32 * 16, true)?;
    gpu.view = Some(view);
    let mut environment = Environment::default();
    let initial = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let atmosphere = *gpu.atmosphere.parameters().words();
    let mapping = *gpu.atmosphere.sky_mapping();
    environment.exposure_ev = 2.;
    let exposed = host.render_environment(&mut gpu, &image, &camera(), environment)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 128);
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
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 192);
    assert!(brightness(&dark) < brightness(&initial));
    let mut exposed_hdr = vec![0; 32 * 32 * 16];
    gpu.view
        .as_ref()
        .unwrap()
        .accumulation
        .read(&mut gpu.context, 0, &mut exposed_hdr)?;
    gpu.restart_accumulation()?;
    gpu.settings.samples = 192;
    environment.exposure_ev = 0.;
    host.render_environment(&mut gpu, &image, &camera(), environment)?;
    let mut reference_hdr = vec![0; exposed_hdr.len()];
    gpu.view
        .as_ref()
        .unwrap()
        .accumulation
        .read(&mut gpu.context, 0, &mut reference_hdr)?;
    assert_eq!(
        exposed_hdr, reference_hdr,
        "曝光不能改变同一绝对样本序列的 HDR"
    );
    drop(gpu);
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
    let mut view = View::new(&mut gpu.context, [32, 32], false)?;
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
    let mut gpu = host.gpu()?;
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
    host.render_options(&mut gpu, &image, &camera, Environment::default(), options)?;
    let brighter = accumulated_rgb(&mut gpu)?;
    assert_eq!(gpu.view.as_ref().unwrap().progress.samples(), 1);
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

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证自动测光只改变宿主呈现、不改变HDR"]
fn hardware_pt_auto_exposure_preserves_hdr() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    let options = Options::new(false, streamline::Quality::Dlaa, 64, 6)?;
    let environment = Environment::default();
    host.render_options(&mut gpu, &image, &camera(), environment, options)?;
    let manual = image.download(&mut gpu.context)?;
    let manual_hdr = exposure_hdr(&mut gpu)?;
    gpu.invalidate()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证强度缩放测光目标并保持适应历史"]
fn hardware_pt_auto_exposure_strength_scales_target_ev() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    freeze_exposure_clock(&mut gpu);
    gpu.set_display_options(1.0, 0.05, 0.75, 0.4)?;
    let options = Options::new(false, streamline::Quality::Dlaa, 16, 6)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let full = exposure_state(&mut gpu)?;
    gpu.set_display_options(0.5, 0.05, 0.75, 0.4)?;
    gpu.invalidate()?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let half = exposure_state(&mut gpu)?;
    assert!(f32::from_bits(full[2]).abs() > 0.01);
    assert!((f32::from_bits(half[2]) - 0.5 * f32::from_bits(full[2])).abs() < 1e-6);
    assert_eq!(full[0], half[0], "零帧间隔保留已适应的曝光");
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证显示曲线只改变呈现"]
fn hardware_pt_drt_controls_only_change_display() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    gpu.set_display_options(0.0, 0.05, 0.75, 0.4)?;
    let options = Options::new(false, streamline::Quality::Dlaa, 16, 6)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let first = image.download(&mut gpu.context)?;
    let hdr = exposure_hdr(&mut gpu)?;
    gpu.set_display_options(0.0, 0.5, 0.0, 0.95)?;
    gpu.invalidate()?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    assert_eq!(hdr, exposure_hdr(&mut gpu)?);
    assert_ne!(first, image.download(&mut gpu.context)?);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证resize保留曝光适应历史"]
fn hardware_pt_auto_exposure_resize_preserves_adaptation() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；验证自动曝光下手动EV只作用于最终呈现"]
fn hardware_pt_auto_exposure_manual_ev_is_display_only() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    let image = image(&mut gpu)?;
    exposure_view(&mut gpu)?;
    gpu.display_timeline.set_enabled(true);
    freeze_exposure_clock(&mut gpu);
    let options = Options::new(false, streamline::Quality::Dlaa, 64, 6)?;
    host.render_options(&mut gpu, &image, &camera(), Environment::default(), options)?;
    let initial = image.download(&mut gpu.context)?;
    let initial_hdr = exposure_hdr(&mut gpu)?;
    let initial_state = exposure_state(&mut gpu)?;
    gpu.invalidate()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追设备；验证无天空维度完全旁路天空与 Aerial"]
fn hardware_pt_disabled_sky() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
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
    let mut gpu = host.gpu()?;
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
    let split = host.render(&mut gpu, &image, &camera())?;
    assert_eq!(
        whole, split,
        "quad 的两半身份、真实四角、UV 与覆盖率不能受表示方式影响"
    );
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "需要 Vulkan 硬件光追；quad 材质字段必须实际影响不透明表面"]
fn hardware_pt_quad_material_preset_is_consumed() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
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
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

fn half_pixel(bytes: &[u8], extent: [u32; 2], components: usize) -> Vec<f32> {
    let center = ((extent[1] / 2 * extent[0] + extent[0] / 2) as usize) * components * 2;
    bytes[center..center + components * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|word| half(u16::from_le_bytes(*word)))
        .collect()
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
    let mut gpu = host.gpu()?;
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
    assert_eq!(view.progress.samples(), 1, "RR仍只累计本帧样本");
    let motion = half_pixel(
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
            .all(|word| u32::from_le_bytes(*word) == 1)
    );

    for event in [GlobalReset::WorldReplaced, GlobalReset::SceneDomainReplaced] {
        gpu.invalidate_history(event)?;
        camera.origin[0] += 0.25;
        host.render_options(&mut gpu, &image, &camera, environment, options)?;
        let motion = half_pixel(
            &gpu.view.as_ref().unwrap().rr.images[2].download(&mut gpu.context)?,
            [512, 512],
            2,
        );
        assert!(
            motion.iter().all(|value| value.abs() < 0.001),
            "封闭全局事件必须重置guide：{motion:?}"
        );
    }
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独立进程运行此 filter：early interposer + PT/guide/RR/超分/呈现完整链路"]
fn hardware_pt_rr_full_chain() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new_rr()?;
    let mut gpu = host.gpu()?;
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
        assert_eq!(view.progress.samples(), 1, "RR 输入每帧独立单样本");
        assert!(
            gpu.rr_previous.is_some(),
            "RR 必须成功求值，不能只走 noisy 回退"
        );
        let images = &view.rr.images;
        let noisy = images[0].download(&mut gpu.context)?;
        let output = images[6].download(&mut gpu.context)?;
        assert!(finite_half_image(&noisy) && finite_half_image(&output));
        let noisy_pixel = half_pixel(&noisy, extent, 4);
        let output_pixel = half_pixel(&output, extent, 4);
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
        let motion = half_pixel(&images[2].download(&mut gpu.context)?, extent, 2);
        let expected = if frame == 1 { 40.0 } else { 0.0 };
        assert!(
            (motion[0] - expected).abs() < 0.1 && motion[1].abs() < 0.1,
            "frame={frame}，运动={motion:?}，预期x={expected}"
        );
        let normal = half_pixel(&images[3].download(&mut gpu.context)?, extent, 4);
        assert!(normal[0].abs() < 1e-4 && normal[1].abs() < 1e-4 && (normal[2] + 1.0).abs() < 1e-4);
        assert!((normal[3] - 0.9).abs() < 0.001);
        let diffuse = half_pixel(&images[4].download(&mut gpu.context)?, extent, 4);
        let specular = half_pixel(&images[5].download(&mut gpu.context)?, extent, 4);
        assert!(diffuse[..3].iter().all(|&v| v > 0.0 && v < 1.0));
        assert!(specular[..3].iter().all(|&v| v > 0.0 && v < 1.0));
        let distance = images[7].download(&mut gpu.context)?;
        let distance = f32::from_le_bytes(distance[center..center + 4].try_into()?);
        assert!((distance - 5.0).abs() < 0.001, "反射段距离：{distance}");
        let reflected = half_pixel(&images[8].download(&mut gpu.context)?, extent, 2);
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
    assert_eq!(view.progress.samples(), 3);
    let input = view.extent;
    for (index, image) in view.rr.images.iter().enumerate() {
        let expected = if index == 6 { extent } else { input };
        assert_eq!(image.extent(), [expected[0], expected[1], 1]);
    }
    let motion = half_pixel(&view.rr.images[2].download(&mut gpu.context)?, input, 2);
    let specular = half_pixel(&view.rr.images[8].download(&mut gpu.context)?, input, 2);
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
            .all(|word| u32::from_le_bytes(*word) == 3)
    );
    let mut accumulation = vec![0; view.accumulation.len() as usize];
    view.accumulation
        .read(&mut gpu.context, 0, &mut accumulation)?;
    let mean = half_pixel(&noisy, input, 4);
    let pixel = ((input[1] / 2 * input[0] + input[0] / 2) as usize) * 16;
    for (channel, mean) in mean[..3].iter().copied().enumerate() {
        let offset = pixel + channel * 4;
        let sum = f32::from_le_bytes(accumulation[offset..offset + 4].try_into()?);
        assert!((mean - sum / 3.0).abs() <= mean.abs().max(1.0) * 0.001);
    }
    let sample = gpu.absolute_sample;
    host.render_options(&mut gpu, &image, &camera, environment, performance)?;
    assert_eq!(gpu.absolute_sample, sample + 3);
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
        let motion = half_pixel(&view.rr.images[2].download(&mut gpu.context)?, expected, 2);
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
    let motion = half_pixel(
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
    let fallback_bindings = Bindings::new(
        &mut gpu.context,
        &[
            Binding::StorageBuffer(view.params),
            Binding::StorageBuffer(view.output),
            Binding::StorageImage(view.rr.images[0]),
            Binding::SampledImage(view.rr.images[6]),
            Binding::StorageImage(resized),
            Binding::SampledImage(view.rr.images[1]),
            Binding::SampledImage(view.rr.images[3]),
            Binding::SampledImage(view.rr.images[4]),
            Binding::SampledImage(view.rr.images[5]),
            Binding::StorageBuffer(view.display.state()),
            Binding::StorageBuffer(view.display.parameters()),
        ],
    )?;
    let fallback = view
        .rr_fallback
        .rebind(&mut gpu.context, fallback_bindings)?;
    let mut commands = Commands::new(&gpu.context)?;
    commands.barrier();
    commands.dispatch(fallback, [1280_u32.div_ceil(8), 720_u32.div_ceil(8), 1])?;
    commands.barrier();
    commands
        .finish(&mut gpu.context)?
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
    gpu.view.as_ref().unwrap().display.parameters().read(
        &mut gpu.context,
        0,
        &mut exposure_frame,
    )?;
    assert_eq!(u32::from_le_bytes(exposure_frame[..4].try_into()?), 1280);
    assert_eq!(u32::from_le_bytes(exposure_frame[4..8].try_into()?), 720);
    // bridge 只允许一个 RR owner；第二会话必须在任何编码前回退，且保留 PT 累积。
    let mut fallback = host.gpu()?;
    let fallback_image = self::image(&mut fallback)?;
    for expected in [1, 2] {
        host.render_rr(&mut fallback, &fallback_image, &camera, environment)?;
        assert!(fallback.rr_unavailable.is_some());
        let view = fallback.view.as_ref().ok_or("回退 PT 未创建视图")?;
        assert!(!view.rr_enabled);
        assert_eq!(view.progress.samples(), expected);
    }
    let pixels = fallback_image.download(&mut fallback.context)?;
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 255)
    );
    drop(fallback);
    drop(gpu);
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
