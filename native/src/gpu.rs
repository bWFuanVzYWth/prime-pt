//! Minecraft 的场景与宿主适配；资源、AS、pipeline/SBT 和命令执行由引擎维护。
//!
//! 每次调用或销毁前宿主须确认前次命令完成，并在调用期间独占队列。
//! 设备/输出导入、宿主命令编码与 引擎 SDK 求值共享同一完成边界。
//! section局部BLAS由scene_cache独占；revision变化只重建对应BLAS，anchor只改变TLAS平移。
//! 一批dirty三角输入连续收集，共享native上传与scratch并只提交等待一次；返回后释放CPU输入。
//! 属性仍按活动槽全量重新打包上传；更新完成前保留旧资源，所有准备成功后发布。

use crate::scene::{Camera, Environment, Mesh};
use crate::{reconstruction::CameraFrame, streamline};
#[path = "exposure.rs"]
mod exposure;
use pt::gpu::{Programs, Renderer};
use vulkan::{
    AccelerationStructure, TriangleInstance,
    interop::RayShader,
    native::{
        Binding, Bindings, Buffer, Capabilities, Commands, ComputePipeline, Context,
        ExternalBinding, ExternalKind, Flight, Image, ImageFormat, RawDevice, Recorded, Sampler,
    },
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    SceneCache(#[from] scene_cache::Error),
    #[error(transparent)]
    Vulkan(#[from] vulkan::Error),
    #[error(transparent)]
    Renderer(#[from] pt::gpu::Error),
    #[error(transparent)]
    Settings(#[from] pt::Error),
    #[error(transparent)]
    Display(#[from] simple_engine::display::Error),
    #[error(transparent)]
    Exposure(#[from] simple_engine::display::exposure::Error),
    #[error(transparent)]
    Atmosphere(#[from] atmosphere::Error),
    #[error(transparent)]
    Camera(#[from] camera::Error),
    #[error(transparent)]
    Streamline(#[from] streamline::Error),
    #[error(transparent)]
    Shader(#[from] shader_build::Error),
    #[error(transparent)]
    Flow(#[from] openpbr::FlowError),
    #[error(transparent)]
    Resources(#[from] openpbr::resource::Error),
    #[error(transparent)]
    MaterialPrograms(#[from] openpbr::program::Error),
    #[error("PT 宿主输入：{0}")]
    Invalid(&'static str),
}

#[derive(Clone, Copy)]
struct Scene {
    top: AccelerationStructure,
    quads: Buffer,
}

struct SectionGpu {
    key: [i32; 3],
    revision: u64,
    range: std::ops::Range<u32>,
    blas: Option<vulkan::TriangleBlas>,
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct SceneBuildMetrics {
    triangle_bytes: u64,
    triangle_capacity_bytes: u64,
    pending_capacity_bytes: u64,
    input_capacity_bytes: u64,
    attribute_capacity_bytes: u64,
    instance_capacity_bytes: u64,
    batch: vulkan::BuildTiming,
}

struct View {
    extent: [u32; 2],
    output_extent: [u32; 2],
    params: Buffer,
    output: Buffer,
    accumulation: Buffer,
    counts: Buffer,
    landing: Vec<Buffer>,
    prefix: Buffer,
    progress: pt::Progress<[u32; 23]>,
    present: ComputePipeline,
    rr_enabled: bool,
    quality: streamline::Quality,
    rr: RrImages,
    pack: ComputePipeline,
    rr_present: ComputePipeline,
    rr_fallback: ComputePipeline,
    display: exposure::Display,
}

/// SDK 借用这些 image，所有权和退役边界仍在 mod 视图内。
/// 顺序与 bridge 的 Resource 数组一致，48 B/输入像素 + 8 B/输出像素；关闭仅保留 1×1 占位。
/// noisy/output 为未曝光的线性 BT.709；depth 为正 view-Z，运动为输入像素
/// previous-current，normal/linear roughness 与方向 albedo 来自首个实际表面。
/// distance 仅用于 mod 诊断；SDK 使用额外理想反射射线生成的局部平面 specular motion。
struct RrImages {
    images: [Image; 9],
}

impl RrImages {
    fn new(context: &mut Context, extent: [u32; 2], output: [u32; 2]) -> Result<Self, Error> {
        let formats = [
            ImageFormat::Rgba16Float,
            ImageFormat::R32Float,
            ImageFormat::Rg16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::R32Float,
            ImageFormat::Rg16Float,
        ];
        let images = formats
            .into_iter()
            .enumerate()
            .map(|(index, format)| {
                let size = if index == 6 { output } else { extent };
                Image::new(context, [size[0], size[1], 1], format)
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .ok()
            .unwrap();
        Ok(Self { images })
    }

    fn resources(&self, context: &Context) -> Result<[streamline::Resource; 9], Error> {
        let raw = self
            .images
            .each_ref()
            .map(|image| image.raw(context))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(raw
            .into_iter()
            .map(|raw| streamline::Resource {
                image: raw.image,
                view: raw.view,
                memory: raw.memory,
                width: raw.extent[0],
                height: raw.extent[1],
                format: raw.format,
                layout: raw.layout,
                usage: raw.usage,
                reserved: 0,
            })
            .collect::<Vec<_>>()
            .try_into()
            .ok()
            .expect("固定九个图像"))
    }
}

impl View {
    fn roots(&self) -> Vec<vulkan::native::Resource> {
        let mut roots = vec![
            self.params.into(),
            self.output.into(),
            self.accumulation.into(),
            self.counts.into(),
            self.prefix.into(),
            self.present.into(),
            self.pack.into(),
            self.rr_present.into(),
            self.rr_fallback.into(),
        ];
        roots.extend(
            self.landing
                .iter()
                .copied()
                .map(vulkan::native::Resource::from),
        );
        roots.extend(
            self.rr
                .images
                .into_iter()
                .map(vulkan::native::Resource::from),
        );
        roots.extend(self.display.roots());
        roots
    }

    #[cfg(test)]
    fn new(context: &mut Context, extent: [u32; 2], rr_enabled: bool) -> Result<Self, Error> {
        Self::new_scaled(
            context,
            extent,
            extent,
            rr_enabled,
            streamline::Quality::Dlaa,
            None,
        )
    }

    fn new_scaled(
        context: &mut Context,
        extent: [u32; 2],
        output_extent: [u32; 2],
        rr_enabled: bool,
        quality: streamline::Quality,
        previous_display: Option<&exposure::Display>,
    ) -> Result<Self, Error> {
        if extent
            .into_iter()
            .chain(output_extent)
            .any(|n| n == 0 || n > 65536)
        {
            return Err(Error::Invalid("视图尺寸须属于 1..=65536"));
        }
        let pixels = u64::from(extent[0]) * u64::from(extent[1]);
        // 当前 opaque/air 的五个交接 plane；格式能力边界见 primary.slang。
        let landing = [16, 16, 16, 8, 4]
            .map(|stride| Buffer::new(context, pixels * stride, false))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let params = Buffer::new(context, 672, true)?;
        let output = Buffer::new(context, pixels * 16, false)?;
        let rr = RrImages::new(
            context,
            if rr_enabled { extent } else { [1, 1] },
            if rr_enabled { output_extent } else { [1, 1] },
        )?;
        let display = exposure::Display::new(
            context,
            output,
            extent,
            rr_enabled.then_some((rr.images[6], output_extent)),
            previous_display,
        )?;
        // 缓存 program 只引用自有占位资源；宿主 view 每帧通过 rebind 借出。
        let placeholder = Image::new(context, [1, 1, 1], ImageFormat::Rgba8Unorm)?;
        let present_bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::StorageBuffer(output),
                Binding::StorageImage(placeholder),
                Binding::StorageBuffer(display.state()),
                Binding::StorageBuffer(display.parameters()),
            ],
        )?;
        let present = ComputePipeline::new(
            context,
            present_bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/present.spv")),
                c"present_main",
            ),
        )?;
        let bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::StorageBuffer(output),
                Binding::StorageImage(rr.images[0]),
                Binding::SampledImage(rr.images[6]),
                Binding::StorageImage(placeholder),
                Binding::SampledImage(rr.images[1]),
                Binding::SampledImage(rr.images[3]),
                Binding::SampledImage(rr.images[4]),
                Binding::SampledImage(rr.images[5]),
                Binding::StorageBuffer(display.state()),
                Binding::StorageBuffer(display.parameters()),
            ],
        )?;
        let pack_bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::StorageBuffer(output),
                Binding::StorageImage(rr.images[0]),
            ],
        )?;
        let pack = ComputePipeline::new(
            context,
            pack_bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/rr_pack.spv")),
                c"pack_main",
            ),
        )?;
        let rr_present = ComputePipeline::new(
            context,
            bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/rr_present.spv")),
                c"present_main",
            ),
        )?;
        let rr_fallback = ComputePipeline::new(
            context,
            bindings,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/rr_fallback.spv")),
                c"fallback_main",
            ),
        )?;
        Ok(Self {
            extent,
            output_extent,
            params,
            output,
            accumulation: Buffer::new(context, pixels * 16, false)?,
            counts: Buffer::new(context, pixels * 4, false)?,
            landing,
            prefix: Buffer::new(context, pixels * 16, false)?,
            progress: pt::Progress::new(),
            present,
            rr_enabled,
            quality,
            rr,
            pack,
            rr_present,
            rr_fallback,
            display,
        })
    }
}

fn shader(spirv: &'static [u8], entry: &'static std::ffi::CStr) -> RayShader<'static> {
    RayShader { spirv, entry }
}

fn programs<'a>(callables: &'a [RayShader<'a>], invocation_reorder: bool) -> Programs<'a> {
    Programs {
        prefix: shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/prefix.spv")),
            c"prefix_main",
        ),
        transport: shader(
            if invocation_reorder {
                include_bytes!(concat!(env!("OUT_DIR"), "/render-ser.spv"))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/render.spv"))
            },
            c"render_main",
        ),
        miss: shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/miss.spv")),
            c"primary_miss",
        ),
        closest_hit: shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/closesthit.spv")),
            c"primary_closesthit",
        ),
        any_hit: Some(shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/anyhit.spv")),
            c"primary_anyhit",
        )),
        callables,
        post: shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/post.spv")),
            c"post_main",
        ),
    }
}

// 每代 atlas、能力证明和已初始化程序一起发布；旧命令通过 Flight 保活旧绑定。
struct MaterialGeneration {
    programs: openpbr::program::ProgramSet,
    program: Renderer,
}

fn atlas_flow(pixels: &[u8]) -> Result<openpbr::FlowKey, Error> {
    use openpbr::resource::{ColorScan, Coverage, Filtering, MaterialBinding, Texels};
    // 源适配只声明实际LOD0/nearest与alpha规则；范围扫描和闭包推导由引擎维护。
    let mut scan = ColorScan::new(Coverage::AlphaAtLeast(0.5), Filtering::Nearest)?;
    scan.extend(Texels::Srgb8Rgba(pixels))?;
    let unit = openpbr::Range::new(0.0, 1.0)?;
    let zero = openpbr::Range::point(0.0)?;
    let ranges = openpbr::MaterialRanges::opaque(openpbr::Range::new(0.25, 0.9)?, zero, zero)?;
    Ok(MaterialBinding::new(
        ranges,
        scan.finish([unit; 3])?,
        openpbr::InterfaceIor::UNKNOWN,
        false,
    )?
    .flow())
}

/// 已校验的 mod 设置；UI 与命令共享 Java 配置，此边界只接受实际支持的范围。
#[derive(Clone, Copy)]
pub(crate) struct Options {
    rr: bool,
    quality: streamline::Quality,
    samples: u32,
    bounces: u32,
}

impl Options {
    pub(crate) fn new(
        rr: bool,
        quality: streamline::Quality,
        samples: u32,
        bounces: u32,
    ) -> Result<Self, Error> {
        if !(1..=64).contains(&samples) {
            return Err(Error::Invalid("SPP 须属于 1..=64"));
        }
        pt::Settings {
            samples,
            max_vertices: bounces,
            ..Default::default()
        }
        .validate()?;
        Ok(Self {
            rr,
            quality,
            samples,
            bounces,
        })
    }
}

pub(crate) struct Gpu {
    // feature 必须先于 SDK 借用的 image 和设备退役。
    rr_runtime: Option<streamline::Runtime>,
    rr_unavailable: Option<streamline::Error>,
    rr_init: streamline::Init,
    rr_previous: Option<CameraFrame>,
    rr_index: u32,
    rr_sample: u32,
    rr_quality: streamline::Quality,
    context: Context,
    scene: Scene,
    section_cache: scene_cache::Cache<SectionGpu>,
    #[cfg(test)]
    scene_build: SceneBuildMetrics,
    // 程序模板只保留初始空场景，不能钉住重载时完整 mesh／AS 的工作集。
    program_scene: Scene,
    atlas: Image,
    energy: Image,
    nearest: Sampler,
    linear: Sampler,
    atmosphere: atmosphere::native::Atmosphere,
    atmosphere_pending: bool,
    solar_luminance: f32,
    view: Option<View>,
    program_view: View,
    compiler: shader_build::RuntimeCompiler,
    generation: Option<MaterialGeneration>,
    renderer: Option<Renderer>,
    flights: Vec<Flight>,
    // 测试宿主持有的native输出；生产输出由宿主管理，不进入资源表。
    #[cfg(test)]
    test_roots: Vec<vulkan::native::Resource>,
    settings: pt::Settings,
    invocation_reorder: bool,
    display_origin: std::time::Instant,
    display_timeline: simple_engine::display::exposure::Timeline,
}

impl Gpu {
    /// # Safety
    /// 句柄属于同一活动 Vulkan 1.2 设备，已启用 RT pipeline、RayQuery 和 device address。
    /// 另须启用 KHR position fetch 扩展及 feature；没有无 position fetch 的回退路径。
    /// 宿主保留设备至本对象销毁，在每次调用和 drop 前等待全部旧命令完成；调用期间独占 queue。
    #[cfg(test)]
    pub(crate) unsafe fn attach(
        instance: u64,
        physical: u64,
        device: u64,
        queue: u64,
        family: u32,
    ) -> Result<Self, Error> {
        // SAFETY: 测试设备只创建 index=0 的 queue，沿用调用者的生命周期契约。
        unsafe { Self::attach_queue(instance, physical, device, queue, family, 0) }
    }

    /// # Safety
    /// 同 attach；family/index 必须精确对应借出的 queue。
    #[cfg(test)]
    pub(crate) unsafe fn attach_queue(
        instance: u64,
        physical: u64,
        device: u64,
        queue: u64,
        family: u32,
        queue_index: u32,
    ) -> Result<Self, Error> {
        // SAFETY: 沿用宿主契约，旧入口不声明额外设备特性。
        unsafe {
            Self::attach_options(
                instance,
                physical,
                device,
                queue,
                family,
                queue_index,
                false,
                true,
            )
        }
    }

    /// # Safety
    /// 句柄属于同一活动设备，已启用 RT pipeline、RayQuery、device address 与 position fetch；family/index
    /// 对应借出的 queue。宿主独占 queue，并在每次调用和销毁前等待前次命令完成。
    /// invocation_reorder 为 true 时设备须另启用 EXT SER 扩展与特性。
    /// ser_requested 为当前偏好；关闭不抹去逻辑设备能力。
    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn attach_options(
        instance: u64,
        physical: u64,
        device: u64,
        queue: u64,
        family: u32,
        queue_index: u32,
        invocation_reorder: bool,
        ser_requested: bool,
    ) -> Result<Self, Error> {
        let raw = RawDevice {
            instance,
            physical_device: physical,
            device,
            queue,
            queue_family: family,
            api_version: (1 << 22) | (4 << 12),
            subgroup_size_control: false,
            capabilities: Capabilities {
                ray_tracing: true,
                position_fetch: true,
                invocation_reorder,
                ..Default::default()
            },
        };
        // SAFETY: 外部生命周期由导出会话契约维持；本对象不销毁宿主句柄。
        let mut context = unsafe { Context::from_raw(raw)? };
        let atlas = Image::new(&mut context, [1, 1, 1], ImageFormat::Rgba8Srgb)?;
        atlas.upload(&mut context, &[255; 4])?;
        let energy = Image::new(
            &mut context,
            openpbr::ENERGY_EXTENT,
            ImageFormat::Rgba16Float,
        )?;
        energy.upload(&mut context, openpbr::energy_bytes())?;
        let nearest = Sampler::new(&mut context, false)?;
        let linear = Sampler::new(&mut context, true)?;
        let scene = Self::build_scene(&mut context, &Mesh::default())?;
        let program_scene = scene;
        let mut atmosphere = atmosphere::native::Atmosphere::new(&mut context)?;
        atmosphere
            .initialize(&mut context)?
            .submit_and_wait(&mut context)?;
        atmosphere.release_initialization();
        let solar = atmosphere.parameters().sun_radiance_rgb();
        let solar_luminance = 0.2627 * solar[0] + 0.6780 * solar[1] + 0.0593 * solar[2];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let compiler = shader_build::RuntimeCompiler::new(
            root.join("../target/shaders"),
            &[&root.join("shaders"), &root.join("../../../shaders")],
            &[],
        )?;
        let program_view = View::new_scaled(
            &mut context,
            [1, 1],
            [1, 1],
            false,
            streamline::Quality::Dlaa,
            None,
        )?;
        let mut gpu = Self {
            rr_runtime: None,
            rr_unavailable: None,
            rr_init: streamline::Init {
                instance,
                physical_device: physical,
                device,
                queue_family: family,
                queue_index,
            },
            rr_previous: None,
            rr_index: 0,
            rr_sample: 0,
            rr_quality: streamline::Quality::Performance,
            context,
            scene,
            section_cache: scene_cache::Cache::new()?,
            #[cfg(test)]
            scene_build: SceneBuildMetrics::default(),
            program_scene,
            atlas,
            energy,
            nearest,
            linear,
            atmosphere,
            atmosphere_pending: false,
            solar_luminance,
            view: None,
            program_view,
            compiler,
            generation: None,
            renderer: None,
            flights: Vec::new(),
            #[cfg(test)]
            test_roots: Vec::new(),
            settings: pt::Settings::default().validate()?,
            invocation_reorder: invocation_reorder && ser_requested,
            display_origin: std::time::Instant::now(),
            display_timeline: simple_engine::display::exposure::Timeline::default(),
        };
        gpu.set_atlas(1, 1, &[255; 4])?;
        Ok(gpu)
    }

    fn build_scene(context: &mut Context, mesh: &Mesh) -> Result<Scene, Error> {
        if mesh.quads.len() > 4 * 1024 * 1024 || mesh.quads.iter().any(|q| !q.valid()) {
            return Err(Error::Invalid("quad 非有限、材质预设无效或超过 4M 面预算"));
        }
        // 稳定记录明确驻留 DeviceLocal；一次 dirty 上传，无 shader 索引和逐帧转置。
        let attributes: Vec<_> = mesh.quads.iter().map(|q| q.attributes()).collect();
        let bytes = bytemuck::cast_slice(&attributes);
        let quads = Buffer::upload(context, if bytes.is_empty() { &[0; 48] } else { bytes })?;
        // AS 构建仍使用两输入槽，不能删退化半面后改变 primitive>>1 身份。
        let triangles: Vec<_> = mesh
            .quads
            .iter()
            .flat_map(|quad| {
                let [a, b, c, d] = quad.positions();
                [[a, b, c], [a, c, d]]
            })
            .collect();
        let instances = if triangles.is_empty() {
            Vec::new()
        } else {
            vec![TriangleInstance {
                geometry: context.triangle_blas(&triangles, false)?,
                transform: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
                custom_index: 0,
            }]
        };
        let top = context.acceleration_instances(&instances)?;
        Ok(Scene { top, quads })
    }

    /// 完成旧提交后切换阶段程序；仅 transport shader 不同，保留数据和 PT/RR 历史。
    /// 不支持时保持普通程序；创建失败保留当前程序和偏好。
    pub(crate) fn set_ser(&mut self, requested: bool) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_ser_inner(requested);
        self.collect_resources()?;
        result
    }

    fn set_ser_inner(&mut self, requested: bool) -> Result<(), Error> {
        let enabled = requested && self.context.capabilities().invocation_reorder;
        if enabled == self.invocation_reorder {
            return Ok(());
        }
        let generation = self.generation.as_ref().unwrap();
        let callables = [RayShader {
            spirv: generation.programs.artifacts()[0].bytes(),
            entry: c"shade_opaque",
        }];
        let descriptors =
            self.renderer_bindings(&self.program_view, &self.atlas, &self.program_scene);
        let (trace, post) = (
            Bindings::new(&mut self.context, &descriptors.0)?,
            Bindings::new(&mut self.context, &descriptors.1)?,
        );
        let program = Renderer::new(
            &mut self.context,
            programs(&callables, enabled),
            trace,
            post,
            [1, 1],
        )?;
        self.generation.as_mut().unwrap().program = program;
        self.invocation_reorder = enabled;
        self.renderer = None;
        Ok(())
    }

    fn invalidate(&mut self) -> Result<(), Error> {
        self.complete_previous()?;
        self.renderer = None;
        self.rr_previous = None;
        if let Some(view) = &mut self.view {
            view.progress.reset();
        }
        Ok(())
    }

    fn complete_previous(&mut self) -> Result<(), Error> {
        for flight in self.flights.drain(..) {
            // SAFETY: 所有公开调用及drop前宿主已经完成此前command。
            unsafe {
                self.context.complete(flight)?;
            }
        }
        if std::mem::take(&mut self.atmosphere_pending) {
            self.atmosphere.submitted();
        }
        self.display_timeline.submitted();
        Ok(())
    }

    pub(crate) fn set_atlas(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_atlas_inner(width, height, pixels);
        self.collect_resources()?;
        result
    }

    fn set_atlas_inner(&mut self, width: u32, height: u32, pixels: &[u8]) -> Result<(), Error> {
        let expected = (u64::from(width) * u64::from(height))
            .checked_mul(4)
            .ok_or(Error::Invalid("atlas 字节数溢出"))?;
        if width == 0 || height == 0 || expected != pixels.len() as u64 {
            return Err(Error::Invalid("atlas 尺寸与 RGBA8 字节数不匹配"));
        }
        // 当前适配器没有转发 mip／动画更新；分析覆盖 shader 实际可读的全部 atlas。
        let flow = atlas_flow(pixels)?;
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/primary.slang");
        let material_programs = openpbr::program::ProgramSet::compile(
            &self.compiler,
            [flow],
            openpbr::program::Template {
                source: source
                    .to_str()
                    .ok_or(Error::Invalid("shader 路径不是 UTF-8"))?,
                entry: "shade_opaque",
                output: "flow.spv",
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &["-fp-mode", "precise"],
            },
        )?;
        let atlas = Image::new(
            &mut self.context,
            [width, height, 1],
            ImageFormat::Rgba8Srgb,
        )?;
        atlas.upload(&mut self.context, pixels)?;
        let descriptors = self.renderer_bindings(&self.program_view, &atlas, &self.program_scene);
        let (trace, post) = self.allocate_bindings(descriptors)?;
        let program = if let Some(previous) = &self.generation
            && material_programs.same_code(&previous.programs)
        {
            previous
                .program
                .rebind(&mut self.context, trace, post, [1, 1])?
        } else {
            let callables = [RayShader {
                spirv: material_programs.artifacts()[0].bytes(),
                entry: c"shade_opaque",
            }];
            Renderer::new(
                &mut self.context,
                programs(&callables, self.invocation_reorder),
                trace,
                post,
                [1, 1],
            )?
        };
        self.invalidate()?;
        self.atlas = atlas;
        self.generation = Some(MaterialGeneration {
            programs: material_programs,
            program,
        });
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_scene(&mut self, mesh: &Mesh) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_scene_inner(mesh);
        self.collect_resources()?;
        result
    }

    #[cfg(test)]
    fn set_scene_inner(&mut self, mesh: &Mesh) -> Result<(), Error> {
        let scene = Self::build_scene(&mut self.context, mesh)?;
        self.invalidate()?;
        self.scene = scene;
        self.section_cache = scene_cache::Cache::new()?;
        Ok(())
    }

    /// 每section复用未变BLAS；anchor只改变TLAS实例平移，不改局部顶点。
    /// 宿主仍在入口前完成旧命令。属性初期按活动槽全量打包上传，未宣称分段上传。
    pub(crate) fn set_sections(&mut self, source: &crate::scene::Scene) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_sections_inner(source);
        self.collect_resources()?;
        result
    }

    fn set_sections_inner(&mut self, source: &crate::scene::Scene) -> Result<(), Error> {
        let mut update = self.section_cache.begin(source.entries().len())?;
        let (slots, entries, _) = update.parts_mut();
        // 输入只包含dirty集；批构建完成后释放CPU三角与描述符，再准备全量属性。
        #[cfg(test)]
        let mut metrics = SceneBuildMetrics::default();
        {
            let (dirty_triangles, dirty_sections) = source.entries().try_fold(
                (0usize, 0usize),
                |(count, sections), (key, revision, mesh)| {
                    if scene_cache::find_revision(
                        self.section_cache.entries(),
                        key,
                        revision,
                        |entry| entry.key,
                        |entry| entry.revision,
                    )
                    .is_some()
                    {
                        Ok((count, sections))
                    } else {
                        let count = count
                            .checked_add(
                                mesh.quads
                                    .len()
                                    .checked_mul(2)
                                    .ok_or(Error::Invalid("dirty三角数量溢出"))?,
                            )
                            .ok_or(Error::Invalid("dirty三角数量溢出"))?;
                        Ok::<_, Error>((count, sections + 1))
                    }
                },
            )?;
            let mut triangles = Vec::new();
            triangles
                .try_reserve_exact(dirty_triangles)
                .map_err(scene_cache::Error::from)?;
            let mut pending = Vec::new();
            pending
                .try_reserve_exact(dirty_sections)
                .map_err(scene_cache::Error::from)?;
            for (key, revision, mesh) in source.entries() {
                if let Some(index) = scene_cache::find_revision(
                    self.section_cache.entries(),
                    key,
                    revision,
                    |entry| entry.key,
                    |entry| entry.revision,
                ) {
                    entries.push(scene_cache::Prepared::Reused(index));
                    continue;
                }
                let count = u32::try_from(mesh.quads.len())
                    .map_err(|_| Error::Invalid("section面索引超过u32"))?;
                let range = slots.take(count)?;
                if range.end > (1 << 24) {
                    return Err(Error::Invalid("section属性槽超过实例24位索引"));
                }
                let start = triangles.len();
                triangles.extend(mesh.quads.iter().flat_map(|quad| {
                    let [a, b, c, d] = quad.positions();
                    [[a, b, c], [a, c, d]]
                }));
                pending.push((entries.len(), start..triangles.len()));
                entries.push(scene_cache::Prepared::Built(SectionGpu {
                    key,
                    revision,
                    range,
                    blas: None,
                }));
            }
            let mut inputs = Vec::new();
            inputs
                .try_reserve_exact(pending.len())
                .map_err(scene_cache::Error::from)?;
            for (_, range) in &pending {
                inputs.push(vulkan::TriangleBlasInput {
                    triangles: &triangles[range.clone()],
                    opaque: false,
                    opacity: None,
                });
            }
            let batch = self.context.triangle_blas_batch(&inputs)?;
            #[cfg(test)]
            {
                metrics.triangle_bytes = std::mem::size_of_val(triangles.as_slice()) as u64;
                metrics.triangle_capacity_bytes =
                    (triangles.capacity() * size_of::<[[f32; 3]; 3]>()) as u64;
                metrics.pending_capacity_bytes =
                    (pending.capacity() * size_of::<(usize, std::ops::Range<usize>)>()) as u64;
                metrics.input_capacity_bytes =
                    (inputs.capacity() * size_of::<vulkan::TriangleBlasInput<'_>>()) as u64;
                metrics.batch = batch.timing;
            }
            for ((index, _), blas) in pending.into_iter().zip(batch.blases) {
                let scene_cache::Prepared::Built(entry) = &mut entries[index] else {
                    unreachable!("dirty输入只对应本批新建记录");
                };
                entry.blas = Some(blas);
            }
        }
        let end = entries
            .iter()
            .map(|entry| entry.get(self.section_cache.entries()).range.end)
            .max()
            .unwrap_or(0);
        let mut attributes = vec![[[0u32; 4]; 3]; end.max(1) as usize];
        let mut instances = Vec::with_capacity(entries.len());
        let anchor = source.anchor();
        for ((key, _, mesh), entry) in source.entries().zip(entries.iter()) {
            let entry = entry.get(self.section_cache.entries());
            for (target, quad) in attributes[entry.range.start as usize..entry.range.end as usize]
                .iter_mut()
                .zip(&mesh.quads)
            {
                *target = quad.attributes();
            }
            let offset: [f32; 3] =
                std::array::from_fn(|axis| (f64::from(key[axis]) * 16.0 - anchor[axis]) as f32);
            instances.push(TriangleInstance {
                geometry: entry
                    .blas
                    .ok_or(Error::Invalid("批构建未完成section BLAS"))?,
                transform: [
                    [1., 0., 0., offset[0]],
                    [0., 1., 0., offset[1]],
                    [0., 0., 1., offset[2]],
                ],
                custom_index: entry.range.start,
            });
        }
        #[cfg(test)]
        {
            metrics.attribute_capacity_bytes =
                (attributes.capacity() * size_of::<[[u32; 4]; 3]>()) as u64;
            metrics.instance_capacity_bytes =
                (instances.capacity() * size_of::<TriangleInstance>()) as u64;
        }
        let quads = Buffer::upload(&mut self.context, bytemuck::cast_slice(&attributes))?;
        let top = self.context.acceleration_instances(&instances)?;
        self.invalidate()?;
        self.section_cache
            .commit(update, |entry| entry.range.clone())?;
        self.scene = Scene { top, quads };
        #[cfg(test)]
        {
            self.scene_build = metrics;
        }

        Ok(())
    }

    // 单场景和两个视图的固定根；录制命令另由Context保护短命绑定。
    fn collect_resources(&mut self) -> Result<(), Error> {
        let mut roots = vec![
            self.scene.top.into(),
            self.scene.quads.into(),
            self.program_scene.top.into(),
            self.program_scene.quads.into(),
            self.atlas.into(),
            self.energy.into(),
            self.nearest.into(),
            self.linear.into(),
        ];
        roots.extend(self.atmosphere.roots());
        roots.extend(self.program_view.roots());
        if let Some(view) = &self.view {
            roots.extend(view.roots());
        }
        if let Some(generation) = &self.generation {
            roots.extend(generation.program.roots());
        }
        if let Some(renderer) = &self.renderer {
            roots.extend(renderer.roots());
        }
        #[cfg(test)]
        roots.extend(self.test_roots.iter().copied());
        self.context.collect(&roots)?;
        Ok(())
    }

    fn renderer_bindings(
        &self,
        view: &View,
        atlas: &Image,
        scene: &Scene,
    ) -> (Vec<Binding>, Vec<Binding>) {
        let buffer = |buffer: &Buffer| Binding::StorageBuffer(*buffer);
        let mut trace = vec![
            Binding::Acceleration(scene.top),
            buffer(&scene.quads),
            Binding::SampledImage(*atlas),
            Binding::Sampler(self.nearest),
            buffer(&view.params),
            Binding::SampledImage(self.energy),
            Binding::Sampler(self.linear),
            buffer(&view.output),
            buffer(&view.accumulation),
            buffer(&view.counts),
        ];
        trace.extend(view.landing.iter().map(buffer));
        trace.push(buffer(&view.prefix));
        trace.extend([
            buffer(self.atmosphere.bindings().sky_mapping),
            Binding::SampledImage(*self.atmosphere.bindings().sky),
            Binding::SampledImage(*self.atmosphere.bindings().optical_depth),
        ]);
        trace.extend([1, 2, 3, 4, 5, 7, 8].map(|i| Binding::StorageImage(view.rr.images[i])));
        let post = vec![
            buffer(&view.params),
            buffer(&view.output),
            buffer(&view.accumulation),
            buffer(&view.counts),
            buffer(&view.prefix),
            Binding::SampledImage(*self.atmosphere.bindings().aerial_light),
            Binding::SampledImage(*self.atmosphere.bindings().aerial_transmittance),
            Binding::Sampler(self.linear),
        ];
        (trace, post)
    }

    fn allocate_bindings(
        &mut self,
        (trace, post): (Vec<Binding>, Vec<Binding>),
    ) -> Result<(Bindings, Bindings), Error> {
        Ok((
            Bindings::new(&mut self.context, &trace)?,
            Bindings::new(&mut self.context, &post)?,
        ))
    }

    fn create_renderer(&mut self) -> Result<Renderer, Error> {
        let view = self.view.as_ref().expect("已创建view");
        let extent = view.extent;
        let descriptors = self.renderer_bindings(view, &self.atlas, &self.scene);
        let (trace, post) = self.allocate_bindings(descriptors)?;
        Ok(self.generation.as_ref().unwrap().program.rebind(
            &mut self.context,
            trace,
            post,
            extent,
        )?)
    }

    #[cfg(test)]
    fn record(
        &mut self,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        target: Binding,
    ) -> Result<Vec<Recorded>, Error> {
        self.record_frame(extent, camera, environment, target, false)
    }

    fn record_frame(
        &mut self,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        target: Binding,
        rr_enabled: bool,
    ) -> Result<Vec<Recorded>, Error> {
        self.complete_previous()?;
        if extent.into_iter().any(|n| n == 0 || n > 65536) {
            return Err(Error::Invalid("视图尺寸须属于 1..=65536"));
        }
        let mut result = Vec::with_capacity(4);
        let recording = (|| -> Result<(), Error> {
            let solar_scale = if environment.enabled() {
                20_000.0 / self.solar_luminance
            } else {
                0.0
            };
            if environment.enabled() {
                self.atmosphere.prepare(
                    &mut self.context,
                    atmosphere::View {
                        altitude_km: (0.5 + (camera.origin[1] - environment.sea_level) * 0.001)
                            .clamp(0.0, 36000.0),
                        forward: camera.forward[..3].try_into().unwrap(),
                        right: camera.right[..3].try_into().unwrap(),
                        up: camera.up[..3].try_into().unwrap(),
                        sun_direction: environment.sun_direction,
                        max_distance_km: 64.0,
                        radiance_scale: [solar_scale; 3],
                    },
                )?;
            }
            if self.view.as_ref().is_none_or(|view| {
                view.output_extent != extent
                    || view.rr_enabled != rr_enabled
                    || (rr_enabled && view.quality != self.rr_quality)
            }) || (rr_enabled && self.rr_runtime.is_none())
            {
                // configure 与替换前，入口契约已确认前次 evaluate 完成。
                let input = if rr_enabled {
                    if self.rr_runtime.is_none() {
                        let init = streamline::Init { ..self.rr_init };
                        self.rr_runtime = Some(unsafe { streamline::Runtime::new(init)? });
                    }
                    self.rr_runtime
                        .as_mut()
                        .unwrap()
                        .configure(extent, self.rr_quality)?
                } else {
                    self.rr_runtime = None;
                    extent
                };
                self.view = Some(View::new_scaled(
                    &mut self.context,
                    input,
                    extent,
                    rr_enabled,
                    self.rr_quality,
                    self.view.as_ref().map(|view| &view.display),
                )?);
                self.rr_previous = None;
                self.renderer = None;
            }
            if self.renderer.is_none() {
                self.renderer = Some(self.create_renderer()?);
            }
            let current =
                CameraFrame::new(camera).map_err(|_| Error::Invalid("RR 相机投影无效"))?;
            if self
                .rr_previous
                .as_ref()
                .is_some_and(|previous| !current.history_compatible(previous))
            {
                self.rr_previous = None;
            }
            let view = self.view.as_mut().unwrap();
            let extent = view.extent;
            let vectors = [camera.origin, camera.forward, camera.right, camera.up];
            let key = std::array::from_fn(|i| {
                if i < 16 {
                    vectors[i / 4][i % 4].to_bits()
                } else {
                    [
                        self.settings.seed,
                        self.settings.max_vertices,
                        self.settings.max_dielectric_events,
                        environment.sun_direction[0].to_bits(),
                        environment.sun_direction[1].to_bits(),
                        environment.sun_direction[2].to_bits(),
                        environment.sea_level.to_bits(),
                    ][i - 16]
                }
            });
            if rr_enabled {
                view.progress.reset();
            }
            let batch = view.progress.begin(key, self.settings.samples)?;
            let mut words: Vec<u32> = vectors.into_iter().flatten().map(f32::to_bits).collect();
            words[3] = environment.sea_level.to_bits();
            words.extend([extent[0], extent[1], batch.first(), batch.samples()]);
            let resolution = extent
                .into_iter()
                .max()
                .unwrap()
                .next_power_of_two()
                .ilog2();
            words.extend([
                self.settings.seed,
                self.settings.max_vertices,
                resolution,
                self.settings.max_dielectric_events,
            ]);
            words.extend(
                simple_engine::display::PrimeDrtSettings::DEFAULT
                    .with_exposure(environment.exposure_ev.exp2())?
                    .prepare(1.0)?
                    .to_array()
                    .map(f32::to_bits),
            );
            words.extend(environment.sun_direction.map(f32::to_bits));
            words.push(if environment.enabled() {
                20_000.0f32.to_bits()
            } else {
                0
            });
            words.extend(self.atmosphere.parameters().words());
            words.extend([solar_scale, solar_scale, solar_scale, 64.0].map(f32::to_bits));
            let jitter = crate::reconstruction::jitter(
                self.rr_index,
                view.extent[0],
                view.output_extent[0],
            )?;
            words.extend(
                [
                    jitter[0],
                    jitter[1],
                    if self.rr_previous.is_some() { 1.0 } else { 0.0 },
                    if rr_enabled { 1.0 } else { 0.0 },
                ]
                .map(f32::to_bits),
            );
            words.extend([self.rr_sample, 0, 0, 0]);
            words.extend(
                self.rr_previous
                    .as_ref()
                    .unwrap_or(&current)
                    .camera_words()
                    .map(f32::to_bits),
            );
            words.extend([view.output_extent[0], view.output_extent[1], 0, 0]);
            view.params
                .write(&mut self.context, 0, bytemuck::cast_slice(&words))?;
            if environment.enabled()
                && let Some(recorded) = self.atmosphere.record(&mut self.context)?
            {
                result.push(recorded);
            }
            if batch.first() == 0 {
                let mut clear = Commands::new(&self.context)?;
                clear.clear_buffer(view.accumulation)?;
                clear.clear_buffer(view.counts)?;
                clear.barrier();
                result.push(clear.finish(&mut self.context)?);
            }
            result.push(
                self.renderer
                    .as_ref()
                    .unwrap()
                    .record(&mut self.context, batch.samples())?,
            );
            if rr_enabled {
                let mut pack = Commands::new(&self.context)?;
                pack.barrier();
                pack.dispatch(view.pack, [extent[0].div_ceil(8), extent[1].div_ceil(8), 1])?;
                pack.barrier();
                result.push(pack.finish(&mut self.context)?);
                batch.commit();
                return Ok(());
            }
            let bindings = Bindings::new(
                &mut self.context,
                &[
                    Binding::StorageBuffer(view.params),
                    Binding::StorageBuffer(view.output),
                    target,
                    Binding::StorageBuffer(view.display.state()),
                    Binding::StorageBuffer(view.display.parameters()),
                ],
            )?;
            let pipeline = view.present.rebind(&mut self.context, bindings)?;
            let frame =
                self.display_timeline
                    .prepare(extent, true, self.display_origin.elapsed())?;
            if let Some(exposure) = view.display.record(
                &mut self.context,
                frame,
                false,
                self.display_timeline.enabled(),
            )? {
                result.push(exposure);
            }
            let mut present = Commands::new(&self.context)?;
            present.barrier();
            present.dispatch(pipeline, [extent[0].div_ceil(8), extent[1].div_ceil(8), 1])?;
            present.barrier();
            result.push(present.finish(&mut self.context)?);
            batch.commit();
            Ok(())
        })();
        if let Err(error) = recording {
            self.display_timeline.cancel();
            // SDK 配置可能已改变；宿主已确认前次完成，下一帧必须重建并重新配置。
            if rr_enabled {
                self.rr_runtime = None;
            }
            self.cancel_recordings(result)?;
            return Err(error);
        }
        Ok(result)
    }

    fn cancel_recordings(&mut self, recordings: Vec<Recorded>) -> Result<(), Error> {
        for recorded in recordings {
            self.context.cancel(recorded)?;
        }
        self.collect_resources()
    }

    /// 宿主独占在 render pass 外录制的 command，且保活命令至 Flight 完成。
    unsafe fn encode_recordings(
        &mut self,
        recordings: impl IntoIterator<Item = Recorded>,
        command: u64,
    ) -> Result<(), Error> {
        let mut pending = recordings.into_iter();
        while let Some(recorded) = pending.next() {
            // SAFETY: 调用者维护宿主命令的独占录制与完成契约。
            match unsafe { recorded.try_encode(&mut self.context, command) } {
                Ok(flight) => self.flights.push(flight),
                Err((error, recorded)) => {
                    self.context.cancel(recorded)?;
                    for recorded in pending {
                        self.context.cancel(recorded)?;
                    }
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    /// 句柄、GENERAL 布局和前次完成保证由 `prime_draw` 的外部契约维护。
    #[allow(
        clippy::too_many_arguments,
        reason = "宿主 command、image/view 与帧输入的直接边界"
    )]
    pub(crate) fn draw(
        &mut self,
        command: u64,
        image: u64,
        view: u64,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        options: Options,
    ) -> Result<(), Error> {
        if [command, image, view].contains(&0) {
            return Err(Error::Invalid("宿主 draw 句柄为空"));
        }
        // SAFETY: prime_draw 借出本设备的 GENERAL storage view，并保活至本次命令完成。
        let target =
            unsafe { ExternalBinding::new(&self.context, view, ExternalKind::StorageImage)? };
        let rr = options.rr;
        let quality_changed = self.rr_quality != options.quality;
        let output_changed = self
            .view
            .as_ref()
            .is_some_and(|view| view.output_extent != extent);
        if self.settings.max_vertices != options.bounces {
            self.rr_previous = None;
        }
        self.settings.samples = options.samples;
        self.settings.max_vertices = options.bounces;
        self.rr_quality = options.quality;
        if !rr || quality_changed || output_changed {
            self.rr_unavailable = None;
        }
        let requested = rr && self.rr_unavailable.is_none();
        // record_frame 不编码宿主 command；SDK 初始化失败仍可安全重新准备原始 PT。
        let mut recordings = match self.record_frame(
            extent,
            camera,
            environment,
            Binding::External(target),
            requested,
        ) {
            Ok(recordings) => recordings,
            Err(Error::Streamline(error)) if requested => {
                eprintln!("RR 不可用，回退 PT：{error}");
                self.rr_unavailable = Some(error);
                self.rr_runtime = None;
                self.record_frame(
                    extent,
                    camera,
                    environment,
                    Binding::External(target),
                    false,
                )?
            }
            Err(error) => return Err(error),
        };
        let result = (|| -> Result<(), Error> {
            let rr = self.view.as_ref().unwrap().rr_enabled;
            let next_index = if rr {
                self.rr_index
                    .checked_add(1)
                    .ok_or(pt::Error::SampleOverflow)?
            } else {
                self.rr_index
            };
            let next_sample = if rr {
                self.rr_sample
                    .checked_add(self.settings.samples)
                    .ok_or(pt::Error::SampleOverflow)?
            } else {
                self.rr_sample
            };
            // SAFETY: 宿主独占正在 render pass 外录制的 command；会话持有 Flight 至完成。
            unsafe {
                self.encode_recordings(std::mem::take(&mut recordings), command)?;
            }
            if rr {
                let current =
                    CameraFrame::new(camera).map_err(|_| Error::Invalid("RR 相机投影无效"))?;
                let reset = self.rr_previous.is_none();
                let view = self.view.as_ref().unwrap();
                let frame = current.frame(
                    command,
                    self.rr_index,
                    reset,
                    crate::reconstruction::jitter(
                        self.rr_index,
                        view.extent[0],
                        view.output_extent[0],
                    )?,
                    self.rr_previous.as_ref().unwrap_or(&current),
                    view.rr.resources(&self.context)?,
                );
                let evaluated = match unsafe { self.rr_runtime.as_mut().unwrap().evaluate(&frame) }
                {
                    Ok(()) => true,
                    Err(error) if error.unsafe_recording() => return Err(error.source.into()),
                    Err(error) => {
                        eprintln!("RR 本帧回退：{error}");
                        false
                    }
                };
                let bindings = Bindings::new(
                    &mut self.context,
                    &[
                        Binding::StorageBuffer(view.params),
                        Binding::StorageBuffer(view.output),
                        Binding::StorageImage(view.rr.images[0]),
                        Binding::SampledImage(view.rr.images[6]),
                        Binding::External(target),
                        Binding::SampledImage(view.rr.images[1]),
                        Binding::SampledImage(view.rr.images[3]),
                        Binding::SampledImage(view.rr.images[4]),
                        Binding::SampledImage(view.rr.images[5]),
                        Binding::StorageBuffer(view.display.state()),
                        Binding::StorageBuffer(view.display.parameters()),
                    ],
                )?;
                let pipeline = if evaluated {
                    view.rr_present.rebind(&mut self.context, bindings)?
                } else {
                    view.rr_fallback.rebind(&mut self.context, bindings)?
                };
                let measurement_extent = if evaluated {
                    view.output_extent
                } else {
                    view.extent
                };
                let frame = self.display_timeline.prepare(
                    measurement_extent,
                    true,
                    self.display_origin.elapsed(),
                )?;
                let exposure = view.display.record(
                    &mut self.context,
                    frame,
                    evaluated,
                    self.display_timeline.enabled(),
                )?;
                recordings.extend(exposure);
                let mut present = Commands::new(&self.context)?;
                present.barrier();
                present.dispatch(pipeline, [extent[0].div_ceil(8), extent[1].div_ceil(8), 1])?;
                present.barrier();
                let present = present.finish(&mut self.context)?;
                recordings.push(present);
                // SAFETY: 同本帧前序命令，宿主持有并独占 command 至完成。
                unsafe {
                    self.encode_recordings(std::mem::take(&mut recordings), command)?;
                }
                self.rr_previous = evaluated.then_some(current);
                self.rr_index = next_index;
                self.rr_sample = next_sample;
            }
            self.collect_resources()?;
            self.atmosphere_pending = true;
            Ok(())
        })();
        if result.is_err() {
            self.display_timeline.cancel();
            self.view.as_mut().unwrap().progress.reset();
            self.rr_previous = None;
            self.cancel_recordings(recordings)?;
        }
        result
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        self.rr_runtime = None;
        if self.complete_previous().is_err() {
            std::process::abort();
        }
    }
}

#[cfg(test)]
#[path = "gpu_smoke.rs"]
mod smoke;

#[cfg(test)]
mod tests {
    #[test]
    fn invisible_atlas_color_cannot_enable_a_diffuse_path() {
        let flow = super::atlas_flow(&[255, 255, 255, 127, 0, 0, 0, 255]).unwrap();
        assert_eq!(
            flow.bits(),
            openpbr::FlowKey::DIELECTRIC_REFLECTION | openpbr::FlowKey::CONTINUOUS_REFLECTION,
        );
    }
}
