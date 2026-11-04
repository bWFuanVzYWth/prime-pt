//! Minecraft 的场景与宿主适配；资源、AS、pipeline/SBT 和命令执行由引擎维护。
//!
//! 每次调用或销毁前宿主须确认前次命令完成，并在调用期间独占队列。
//! 设备和输出导入、宿主命令编码与引擎 SDK 求值共享同一完成边界。
//! 64m region局部BLAS由scene_cache独占；revision变化只重建对应BLAS，anchor只改变TLAS平移。
//! 一批dirty三角输入连续收集，共享native上传与scratch并只提交等待一次；返回后释放CPU输入。
//! 属性存储保留容量并仅写变化范围；全部准备与目录校验后原子写入，再进行不失败的发布。
//!
//! 首次加载和资源重载先准备材质范围、静态发光 alias、Slang callable 及普通／SER 管线与 SBT，
//! 成功后一次发布资源世代，失败保留上一代。切换已支持的 SER 变体只选择现有程序。
//! region 增量更新复用已准备的材质资源，旧 GPU 资源在最后一次实际使用完成后退役。
//! 局部几何变动按 region 重建 BLAS，纯 tint 走属性更新，anchor 变化更新 TLAS 平移。

use crate::scene::{Camera, Environment, Mesh};
use prime_scene::MaterialDefinition;
#[path = "attribute_update.rs"]
mod attribute_update;
#[path = "lighting.rs"]
mod lighting;
#[path = "materials.rs"]
mod materials;
#[path = "region_match.rs"]
mod region_match;
use crate::{reconstruction::CameraFrame, streamline};
use std::path::{Path, PathBuf};
#[path = "exposure.rs"]
mod exposure;
#[path = "history.rs"]
mod history;
pub use history::GlobalReset;
use pt::gpu::{Programs, Renderer};
use vulkan::{
    AccelerationStructure, TriangleInstance,
    interop::RayShader,
    native::{
        Binding, Bindings, Buffer, Capabilities, Commands, ComputePipeline, Context,
        ExternalBinding, ExternalKind, Flight, Image, ImageFormat, RawDevice, Recorded, Sampler,
    },
};

/// 渲染输入、资源、程序或SDK失败；保留实际底层来源。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 资源槽或发布事务失败。
    #[error(transparent)]
    SceneCache(#[from] scene_cache::Error),
    /// 场景输入校验失败。
    #[error(transparent)]
    Scene(#[from] prime_scene::Error),
    /// Vulkan资源或命令失败。
    #[error(transparent)]
    Vulkan(#[from] vulkan::Error),
    /// 通用PT阶段创建或录制失败。
    #[error(transparent)]
    Renderer(#[from] pt::gpu::Error),
    /// 积分预算无效。
    #[error(transparent)]
    Settings(#[from] pt::Error),
    /// 显示设置无效。
    #[error(transparent)]
    Display(#[from] simple_engine::display::Error),
    /// 曝光时间线或参数无效。
    #[error(transparent)]
    Exposure(#[from] simple_engine::display::exposure::Error),
    /// 大气资源创建或更新失败。
    #[error(transparent)]
    Atmosphere(#[from] atmosphere::Error),
    /// 相机投影或坐标转换失败。
    #[error(transparent)]
    Camera(#[from] camera::Error),
    /// SDK初始化或求值失败。
    #[error(transparent)]
    Streamline(#[from] streamline::Error),
    /// 受控Slang编译失败。
    #[error(transparent)]
    Shader(#[from] shader_build::Error),
    /// 材质能力组合无效。
    #[error(transparent)]
    Flow(#[from] openpbr::FlowError),
    /// 材质源资源校验失败。
    #[error(transparent)]
    Resources(#[from] openpbr::resource::Error),
    /// 材质特化程序创建失败。
    #[error(transparent)]
    MaterialPrograms(#[from] openpbr::program::Error),
    /// 光源选择或面积输入无效。
    #[error(transparent)]
    Lights(#[from] light_sampling::Error),
    /// 无法读取开发宿主的工作目录。
    #[error("无法读取开发宿主的工作目录")]
    DevelopmentDirectory(#[source] std::io::Error),
    /// 宿主输入不符合当前渲染契约。
    #[error("PT 宿主输入：{0}")]
    Invalid(&'static str),
}

#[derive(Clone, Copy)]
struct Scene {
    top: AccelerationStructure,
    quads: Buffer,
    instance_quad_bases: Buffer,
}

/// 首次加载或重载的一份完整只读资源输入；借用在准备调用返回时结束。
/// 静态alias由引擎EmitterAlias构建，offset编码在材质normalFlags高31位。
pub struct ResourceUpload<'a> {
    /// Minecraft资源所有者发行的非零世代，与region视图共同验证。
    pub generation: u64,
    /// 所有图像共用的atlas宽高。
    pub extent: [u32; 2],
    /// sRGB RGBA8颜色atlas。
    pub albedo: &'a [u8],
    /// 资源世代内完整、稳定的材质索引表。
    pub materials: &'a [MaterialDefinition],
    /// 原位线性RGBA8 MERS atlas。
    pub mers: &'a [u8],
    /// 原位线性RGBA8辅助材质atlas。
    pub auxiliary: &'a [u8],
    /// 全局唯一的静态发光采样表；offset0保留为uniform fallback。
    pub emission_alias_words: &'a [u32],
    /// 与材质等长的静态quad面积分布；过程几何以首word=0xffffffff声明动态面积。
    pub light_areas: &'a [[u32; 4]],
}

#[derive(Clone, Copy)]
struct MaterialGpu {
    table: Buffer,
    mers: Image,
    auxiliary: Image,
    emission_alias: Buffer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LightingGpu {
    alias: Buffer,
    meta: Buffer,
    vertices: Buffer,
    inverse: Buffer,
    regions: Buffer,
}

impl LightingGpu {
    fn upload(context: &mut Context, tables: Option<&lighting::Tables>) -> Result<Self, Error> {
        let upload = |context: &mut Context, data: &[u8]| -> Result<_, Error> {
            let data = if data.is_empty() { &[0; 16] } else { data };
            let buffer = Buffer::mapped_device_local(context, data.len() as u64)?;
            buffer.write(context, 0, data)?;
            Ok(buffer)
        };
        Ok(Self {
            alias: upload(
                context,
                tables.map_or(&[0u8; 16], |t| bytemuck::cast_slice(&t.selection)),
            )?,
            meta: upload(
                context,
                tables.map_or(&[], |t| bytemuck::cast_slice(&t.meta)),
            )?,
            vertices: upload(
                context,
                tables.map_or(&[], |t| bytemuck::cast_slice(&t.vertices)),
            )?,
            inverse: upload(
                context,
                tables.map_or(&[], |t| bytemuck::cast_slice(&t.inverse)),
            )?,
            regions: upload(
                context,
                tables.map_or(&[], |t| bytemuck::cast_slice(&t.regions)),
            )?,
        })
    }
    fn roots(self) -> [vulkan::native::Resource; 5] {
        [
            self.alias.into(),
            self.meta.into(),
            self.vertices.into(),
            self.inverse.into(),
            self.regions.into(),
        ]
    }
}

struct RegionGpu {
    key: [i32; 3],
    revision: u64,
    range: std::ops::Range<u32>,
    blas: Option<vulkan::TriangleBlas>,
    opaque: bool,
}

/// 单次成功region发布的主机墙钟与工作集；三个主阶段互斥，细分字段仅作诊断。
/// 不包含宿主旧帧完成等待，也不是GPU timestamp；失败更新不发布快照。
#[derive(Clone, Copy, Default)]
pub struct SceneUpdateTiming {
    /// 源Mesh到dirty三角/输入及属性编码；不含AS构建、属性上传和发布。
    pub geometry_ns: u64,
    /// 是否发布了GPU场景或资源绑定；管理器空发布为false。
    pub changed: bool,
    /// BLAS与TLAS构建的主机墙钟，含同步提交与完成等待。
    pub acceleration_ns: u64,
    /// 属性上传及目录/场景发布；与其余两个主阶段互斥。
    pub resources_ns: u64,
    /// 变化属性与必要TLAS实例的CPU准备，已计入geometry_ns。
    pub attribute_pack_ns: u64,
    /// 属性及实例基址缓冲分配和上传，已计入resources_ns。
    pub attribute_upload_ns: u64,
    /// 实际写入的变化属性和实例基址字节，按活动范围计量。
    pub attribute_upload_bytes: u64,
    /// 属性缓冲扩容时设备内保留的前缀字节，独立于宿主写入字节。
    pub attribute_copy_bytes: u64,
    /// 活动光源注册及其目录编码的CPU准备，已计入geometry_ns。
    pub light_pack_ns: u64,
    /// 活动光源buffer分配/写入，已计入resources_ns；静态alias不在此路径构建。
    pub light_upload_ns: u64,
    /// 本次实际写入的活动光源数据字节；保持原分布时为零。
    pub light_upload_bytes: u64,
    /// 本次重新构建的非空region数；属性刷新或纯anchor重定位为零。
    pub dirty_regions: usize,
    /// 本次TLAS使用的活动非空region实例数。
    pub instances: usize,
    /// TLAS准备的主机墙钟，已计入acceleration_ns。
    pub tlas_ns: u64,
    /// 加速结构共同提交与完成等待，已计入acceleration_ns。
    pub as_submit_wait_ns: u64,
    /// 缓存和场景发布，已计入resources_ns。
    pub publish_ns: u64,
    /// 旧资源目录回收，已计入resources_ns。
    pub collect_ns: u64,
    /// dirty region三角有效字节。
    pub triangle_bytes: u64,
    /// dirty region三角容器容量字节。
    pub triangle_capacity_bytes: u64,
    /// 待回填region目录容量字节。
    pub pending_capacity_bytes: u64,
    /// BLAS输入描述符容量字节。
    pub input_capacity_bytes: u64,
    /// 变化属性打包的临时容量字节。
    pub attribute_capacity_bytes: u64,
    /// 全活动TLAS实例临时容量字节。
    pub instance_capacity_bytes: u64,
    /// BLAS批次细分；submit_wait_ns包含BLAS和TLAS的共同等待，已计入acceleration_ns。
    pub batch: vulkan::BuildTiming,
}

fn elapsed_ns(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
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
    progress: pt::Progress<[u32; 25]>,
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
        templates: Option<&Self>,
    ) -> Result<Self, Error> {
        if extent
            .into_iter()
            .chain(output_extent)
            .any(|n| n == 0 || n > 65536)
        {
            return Err(Error::Invalid("视图尺寸须属于 1..=65536"));
        }
        let pixels = u64::from(extent[0]) * u64::from(extent[1]);
        // 五个命中plane另加16B identity；着色法线/纹理与发光反向PDF使用原命中身份。
        let landing = [16, 16, 16, 8, 4, 16]
            .map(|stride| Buffer::new(context, pixels * stride, false))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let params = Buffer::new(context, 704, true)?;
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
            Some((
                rr.images[6],
                if rr_enabled { output_extent } else { [1, 1] },
            )),
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
        let present = if let Some(templates) = templates {
            templates.present.rebind(context, present_bindings)?
        } else {
            ComputePipeline::new(
                context,
                present_bindings,
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/present.spv")),
                    c"present_main",
                ),
            )?
        };
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
        let pack = if let Some(templates) = templates {
            templates.pack.rebind(context, pack_bindings)?
        } else {
            ComputePipeline::new(
                context,
                pack_bindings,
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_pack.spv")),
                    c"pack_main",
                ),
            )?
        };
        let rr_present = if let Some(templates) = templates {
            templates.rr_present.rebind(context, bindings)?
        } else {
            ComputePipeline::new(
                context,
                bindings,
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_present.spv")),
                    c"present_main",
                ),
            )?
        };
        let rr_fallback = if let Some(templates) = templates {
            templates.rr_fallback.rebind(context, bindings)?
        } else {
            ComputePipeline::new(
                context,
                bindings,
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_fallback.spv")),
                    c"fallback_main",
                ),
            )?
        };
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

fn programs<'a>(
    callables: &'a [RayShader<'a>],
    invocation_reorder: bool,
    hit: materials::HitFeatures,
) -> Programs<'a> {
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
            if hit.authored_normals {
                include_bytes!(concat!(env!("OUT_DIR"), "/closesthit.spv"))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/closesthit-flat.spv"))
            },
            c"primary_closesthit",
        ),
        any_hit: (hit.coverage != 0).then(|| {
            shader(
                match hit.coverage {
                    MaterialDefinition::CUTOUT => {
                        include_bytes!(concat!(env!("OUT_DIR"), "/anyhit-cutout.spv"))
                    }
                    MaterialDefinition::FRONT_ONLY => {
                        include_bytes!(concat!(env!("OUT_DIR"), "/anyhit-front.spv"))
                    }
                    _ => include_bytes!(concat!(env!("OUT_DIR"), "/anyhit.spv")),
                },
                c"primary_anyhit",
            )
        }),
        callables,
        post: shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/post.spv")),
            c"post_main",
        ),
    }
}

// 每代 atlas、能力证明和已初始化程序一起发布；旧命令通过 Flight 保活旧绑定。
struct MaterialGeneration {
    programs: openpbr::program::ProgramSet<(openpbr::FlowKey, u32)>,
    hit: materials::HitFeatures,
    regular: Renderer,
    reordered: Option<Renderer>,
}

#[cfg(test)]
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
/// 已校验的逐帧积分与重建设置；不拥有SDK或资源。
pub struct Options {
    rr: bool,
    quality: streamline::Quality,
    samples: u32,
    bounces: u32,
}

impl Options {
    /// 校验SPP和路径预算；SDK档位只记录选择，在实际视图创建时协商。
    pub fn new(
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

/// 独占场景、管线、工作缓冲和SDK；所有入口及销毁前宿主须完成旧命令并独占队列。
pub struct Gpu {
    // feature 必须先于 SDK 借用的 image 和设备退役。
    rr_runtime: Option<streamline::Runtime>,
    rr_unavailable: Option<streamline::Error>,
    rr_init: streamline::Init,
    rr_previous: Option<CameraFrame>,
    rr_index: u32,
    rr_sample: u32,
    rr_quality: streamline::Quality,
    context: Context,
    attribute_slot_limit: u32,
    scene: Scene,
    anchor: [f64; 3],
    region_cache: scene_cache::Cache<RegionGpu>,
    attribute_revisions: Vec<([i32; 3], u64)>,
    scene_update: Option<SceneUpdateTiming>,
    // 程序模板只保留初始空场景，不能钉住重载时完整 mesh／AS 的工作集。
    program_scene: Scene,
    atlas: Image,
    atlas_colors: [openpbr::resource::ColorRanges; 2],
    material_gpu: MaterialGpu,
    material_coverage: Vec<bool>,
    light_sides: Vec<u32>,
    has_emission: bool,
    light_areas: Vec<[u32; 4]>,
    map_ranges: materials::MapRanges,
    resource_generation: u64,
    lighting: lighting::Lighting,
    lighting_gpu: LightingGpu,
    program_lighting: LightingGpu,
    lighting_dirty: bool,
    initial_medium: [f32; 4],
    energy: Image,
    nearest: Sampler,
    linear: Sampler,
    atmosphere: atmosphere::native::Atmosphere,
    atmosphere_pending: bool,
    solar_reference_scale: f32,
    sun_gain: f32,
    block_emission_gain: f32,
    view: Option<View>,
    program_view: View,
    compiler: shader_build::RuntimeCompiler,
    shader_source: PathBuf,
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
    display_drt: simple_engine::display::PrimeDrtParameters,
}

impl Gpu {
    /// 单个 quad 属性缓冲允许的槽数，按分配器使用的二次幂容量向下取整。
    /// 空洞、场景内其他缓冲和实际可用显存另计。
    pub const fn attribute_slot_limit(&self) -> u32 {
        self.attribute_slot_limit
    }

    /// # Safety
    /// 句柄属于同一活动 Vulkan 1.2 设备，已启用 RT pipeline、RayQuery 和 device address。
    /// 另须启用 KHR position fetch 扩展及 feature；没有无 position fetch 的回退路径。
    /// 宿主保留设备至本对象销毁，在每次调用和 drop 前等待全部旧命令完成；调用期间独占 queue。
    #[cfg(test)]
    pub unsafe fn attach(
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
    pub unsafe fn attach_queue(
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
                None,
            )
        }
    }

    /// `runtime_root` 指向已提取并校验的发行资源。开发宿主传 `None`，从当前工作目录向上定位项目源码并使用本机 Slang。
    /// 发行资源的两套 shader 源码在会话期间保持有效，派生缓存写入同根 `cache/`。
    /// # Safety
    /// 句柄属于同一活动设备，已启用 RT pipeline、RayQuery、device address 与 position fetch；family/index
    /// 对应借出的 queue。宿主独占 queue，并在每次调用和销毁前等待前次命令完成。
    /// invocation_reorder 为 true 时设备须另启用 EXT SER 扩展与特性。
    /// ser_requested 为当前偏好；关闭不抹去逻辑设备能力。
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn attach_options(
        instance: u64,
        physical: u64,
        device: u64,
        queue: u64,
        family: u32,
        queue_index: u32,
        invocation_reorder: bool,
        ser_requested: bool,
        runtime_root: Option<&Path>,
    ) -> Result<Self, Error> {
        let (cache, prime_shaders, engine_shaders) = if let Some(root) = runtime_root {
            (
                root.join("cache"),
                root.join("shaders/prime"),
                root.join("shaders/engine"),
            )
        } else {
            let current = std::env::current_dir().map_err(Error::DevelopmentDirectory)?;
            let root = current
                .ancestors()
                .find(|path| path.join("crates/renderer/shaders/primary.slang").is_file())
                .ok_or(Error::Invalid("开发宿主须从 Prime PT 源码目录启动"))?;
            (
                root.join("target"),
                root.join("crates/renderer/shaders"),
                root.join("../../shaders"),
            )
        };
        let includes = [prime_shaders.as_path(), engine_shaders.as_path()];
        let shader_cache = cache.join("shaders");
        let compiler = if let Some(root) = runtime_root {
            shader_build::RuntimeCompiler::from_executable(
                &root.join("slang/slangc.exe"),
                shader_cache,
                &includes,
                &[],
            )?
        } else {
            shader_build::RuntimeCompiler::new(shader_cache, &includes, &[])?
        };
        let shader_source = prime_shaders.join("primary.slang");
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
        context.enable_pipeline_cache(&cache.join("pipeline-cache"));
        let attribute_slots =
            context.max_storage_buffer_range() / size_of::<[[u32; 4]; 3]>() as u32;
        let attribute_slot_limit = attribute_slots
            .checked_ilog2()
            .map_or(0, |shift| 1 << shift);
        let atlas = Image::new(&mut context, [1, 1, 1], ImageFormat::Rgba8Srgb)?;
        atlas.upload(&mut context, &[255; 4])?;
        let energy = Image::new(
            &mut context,
            openpbr::ENERGY_EXTENT,
            ImageFormat::Rgba16Float,
        )?;
        energy.upload(&mut context, openpbr::energy_bytes())?;
        let definitions = materials::defaults();
        let mers = Image::new(&mut context, [1, 1, 1], ImageFormat::Rgba8Unorm)?;
        mers.upload(&mut context, &[0, 0, 255, 0])?;
        let auxiliary = Image::new(&mut context, [1, 1, 1], ImageFormat::Rgba8Unorm)?;
        auxiliary.upload(&mut context, &[128, 128, 255, 128])?;
        let material_gpu = MaterialGpu {
            table: Buffer::upload(&mut context, bytemuck::cast_slice(&definitions))?,
            mers,
            auxiliary,
            emission_alias: Buffer::upload(&mut context, &[0; 16])?,
        };
        let lighting_gpu = LightingGpu::upload(&mut context, None)?;
        let nearest = Sampler::new(&mut context, false)?;
        let linear = Sampler::new(&mut context, true)?;
        let scene = Self::build_scene(&mut context, &Mesh::default())?;
        let program_scene = scene;
        let mut atmosphere = atmosphere::native::Atmosphere::new(&mut context)?;
        atmosphere
            .initialize(&mut context)?
            .submit_and_wait(&mut context)?;
        atmosphere.release_initialization();
        let solar = atmosphere.parameters().solar_irradiance_rgb();
        // 太阳基准为大气外法向辐照度 Y=8，太阳盘辐亮度由 shader 推导。
        let solar_y = 0.262_700_2 * solar[0] + 0.677_998_07 * solar[1] + 0.059_301_715 * solar[2];
        let program_view = View::new_scaled(
            &mut context,
            [1, 1],
            [1, 1],
            false,
            streamline::Quality::Dlaa,
            None,
            None,
        )?;
        let mut display_timeline = simple_engine::display::exposure::Timeline::default();
        display_timeline
            .set_compensation(simple_engine::display::exposure::DEFAULT_COMPENSATION)?;
        let gpu = Self {
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
            attribute_slot_limit,
            scene,
            anchor: [0.0; 3],
            region_cache: scene_cache::Cache::new()?,
            attribute_revisions: Vec::new(),
            scene_update: None,
            program_scene,
            atlas,
            atlas_colors: materials::colors(&[255; 4])?,
            material_gpu,
            material_coverage: definitions.iter().map(materials::tests_coverage).collect(),
            light_areas: vec![[u32::MAX, 0, 0, 0]; definitions.len()],
            light_sides: vec![0; definitions.len()],
            has_emission: false,
            map_ranges: materials::map_ranges(&[0, 0, 255, 0])?,
            resource_generation: 0,
            lighting: lighting::Lighting::new()?,
            lighting_gpu,
            program_lighting: lighting_gpu,
            lighting_dirty: true,
            initial_medium: [1., 0., 0., 0.],
            energy,
            nearest,
            linear,
            atmosphere,
            atmosphere_pending: false,
            solar_reference_scale: 8.0 / solar_y,
            sun_gain: 1.0,
            block_emission_gain: 1.0,
            view: None,
            program_view,
            compiler,
            shader_source,
            generation: None,
            renderer: None,
            flights: Vec::new(),
            #[cfg(test)]
            test_roots: Vec::new(),
            settings: pt::Settings::default().validate()?,
            invocation_reorder: invocation_reorder && ser_requested,
            display_origin: std::time::Instant::now(),
            display_timeline,
            display_drt: simple_engine::display::PrimeDrtSettings::DEFAULT.prepare(1.0)?,
        };
        #[cfg(test)]
        let gpu = {
            let mut gpu = gpu;
            gpu.set_atlas(1, 1, &[255; 4])?;
            gpu
        };
        Ok(gpu)
    }

    /// 更新自动测光强度与 SDR 显示曲线，手动 EV 仍取自帧环境。
    /// 只更新 CPU 设置，保留 PT/RR 历史与已准备的管线。
    /// # Errors
    /// 强度或 DRT 参数无效时保留当前设置。
    pub fn set_display_options(
        &mut self,
        auto_exposure: f32,
        saturation: f32,
        hue: f32,
        compression_start: f32,
    ) -> Result<(), Error> {
        let drt =
            simple_engine::display::PrimeDrtSettings::new(1.0, hue, saturation, compression_start)?
                .prepare(1.0)?;
        self.display_timeline.set_compensation(auto_exposure)?;
        self.display_drt = drt;
        Ok(())
    }

    /// 设置太阳与局部材质发光的场景线性倍率，以 EV 表示。
    /// 太阳倍率也作用于天空散射与 Aerial 辉光。两个值只更新 CPU 帧参数，
    /// 发生有效的输运变化时重启原始 PT 累积，保留 RR 历史和已准备的灯表。
    /// # Errors
    /// 太阳 EV 超出 −8..=8 或方块发光 EV 超出 −10..=10 时保持原设置。
    pub fn set_lighting_options(
        &mut self,
        sun_ev: f32,
        block_emission_ev: f32,
    ) -> Result<(), Error> {
        if !(-8.0..=8.0).contains(&sun_ev) {
            return Err(Error::Invalid("太阳强度须为 −8..=8 EV"));
        }
        if !(-10.0..=10.0).contains(&block_emission_ev) {
            return Err(Error::Invalid("方块发光强度须为 −10..=10 EV"));
        }
        self.sun_gain = sun_ev.exp2();
        self.block_emission_gain = block_emission_ev.exp2();
        Ok(())
    }

    fn build_scene(context: &mut Context, mesh: &Mesh) -> Result<Scene, Error> {
        if mesh.quads.len() > 4 * 1024 * 1024 || mesh.quads.iter().any(|q| !q.valid()) {
            return Err(Error::Invalid("quad 非有限、材质预设无效或超过 4M 面预算"));
        }
        // 稳定记录明确驻留 DeviceLocal；一次 dirty 上传，无 shader 索引和逐帧转置。
        let attributes: Vec<_> = mesh.quads.iter().map(|q| q.attributes()).collect();
        let bytes = bytemuck::cast_slice(&attributes);
        let quads = Buffer::mapped_device_local(context, bytes.len().max(48) as u64)?;
        quads.write(context, 0, if bytes.is_empty() { &[0; 48] } else { bytes })?;
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
        let instance_quad_bases = Buffer::upload(context, &[0; size_of::<u32>()])?;
        Ok(Scene {
            top,
            quads,
            instance_quad_bases,
        })
    }

    /// 完成旧提交后切换阶段程序；仅 transport shader 不同，保留数据和 PT/RR 历史。
    /// 不支持时保持普通程序；仅选择资源边界已准备的变体，不创建pipeline或SBT。
    pub fn set_ser(&mut self, requested: bool) -> Result<(), Error> {
        if (requested && self.context.capabilities().invocation_reorder) == self.invocation_reorder
        {
            return Ok(());
        }
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
        if enabled
            && self.generation.is_some()
            && self
                .generation
                .as_ref()
                .and_then(|g| g.reordered.as_ref())
                .is_none()
        {
            return Err(Error::Invalid("资源准备未提供SER程序"));
        }
        self.invocation_reorder = enabled;
        self.renderer = None;
        Ok(())
    }

    /// 资源变化后重绑阶段并重启静态PT积分；RR重投影与曝光适应继续使用已接受历史。
    fn invalidate(&mut self) -> Result<(), Error> {
        self.complete_previous()?;
        self.renderer = None;
        if let Some(view) = &mut self.view {
            view.progress.reset();
        }
        Ok(())
    }

    /// 调用者确认整个历史域失效；普通section、相机、照明和积分设置变化不调用。
    /// 保留曝光适应状态；入口仍要求宿主先完成旧命令并独占队列。
    pub fn invalidate_history(&mut self, reason: GlobalReset) -> Result<(), Error> {
        self.invalidate()?;
        history::record_global(reason, self.rr_previous.is_some());
        self.rr_previous = None;
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

    /// 完成旧命令后原子替换atlas与其特化程序；失败保留上一代。
    #[cfg(test)]
    fn set_atlas(&mut self, width: u32, height: u32, pixels: &[u8]) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_atlas_inner(width, height, pixels);
        self.collect_resources()?;
        result
    }

    #[cfg(test)]
    fn set_atlas_inner(&mut self, width: u32, height: u32, pixels: &[u8]) -> Result<(), Error> {
        let expected = (u64::from(width) * u64::from(height))
            .checked_mul(4)
            .ok_or(Error::Invalid("atlas 字节数溢出"))?;
        if width == 0 || height == 0 || expected != pixels.len() as u64 {
            return Err(Error::Invalid("atlas 尺寸与 RGBA8 字节数不匹配"));
        }
        let colors = materials::colors(pixels)?;
        let definitions = materials::defaults();
        let flows = materials::flows(colors, &definitions, self.map_ranges)?;
        let atlas = Image::new(
            &mut self.context,
            [width, height, 1],
            ImageFormat::Rgba8Srgb,
        )?;
        atlas.upload(&mut self.context, pixels)?;
        let (resources, generation) =
            self.prepare_material_generation(atlas, self.material_gpu, &flows, &definitions)?;
        self.invalidate()?;
        self.atlas = atlas;
        self.atlas_colors = colors;
        self.material_gpu = resources;
        self.material_coverage = definitions.iter().map(materials::tests_coverage).collect();
        self.generation = Some(generation);
        Ok(())
    }

    /// 完整资源世代的唯一准备与发布入口；不得由section、实例或逐帧路径调用。
    ///
    /// 全部图像、静态alias、材质范围、程序和可用SER变体成功后一次发布。
    /// 旧命令须已完成；调用后在绘制前提交同一资源世代的region视图。
    pub fn replace_resources(&mut self, source: ResourceUpload<'_>) -> Result<(), Error> {
        self.complete_previous()?;
        let result = (|| {
            let bytes = u64::from(source.extent[0])
                .checked_mul(u64::from(source.extent[1]))
                .and_then(|n| n.checked_mul(4))
                .ok_or(Error::Invalid("资源atlas尺寸溢出"))?;
            if source.generation <= self.resource_generation
                || source.extent.iter().any(|&v| v == 0 || v > 16384)
                || [
                    source.albedo.len(),
                    source.mers.len(),
                    source.auxiliary.len(),
                ]
                .into_iter()
                .any(|n| n as u64 != bytes)
                || source.materials.is_empty()
                || source.materials.len() != source.light_areas.len()
                || source.materials.iter().any(|m| !m.valid())
            {
                return Err(Error::Invalid("完整资源图像或材质域无效"));
            }
            validate_aliases(source.materials, source.emission_alias_words)?;
            for area in source.light_areas {
                if area[0] != u32::MAX
                    && (area[0] > light_sampling::SELECTORS
                        || area[1..3]
                            .iter()
                            .any(|&v| !f32::from_bits(v).is_finite() || f32::from_bits(v) < 0.))
                {
                    return Err(Error::Invalid("静态quad面积分布无效"));
                }
            }
            let region_cache = scene_cache::Cache::new()?;
            let lighting = lighting::Lighting::new()?;
            let definitions = source.materials;
            let colors = materials::colors(source.albedo)?;
            let ranges = materials::map_ranges(source.mers)?;
            let flows = materials::flows(colors, definitions, ranges)?;
            let mut light_sides = Vec::new();
            light_sides
                .try_reserve_exact(source.materials.len())
                .map_err(scene_cache::Error::from)?;
            light_sides.extend(definitions.iter().zip(&flows).map(|(m, flow)| {
                if flow.emits() {
                    materials::emission_sides(m)
                } else {
                    0
                }
            }));
            let has_emission = light_sides.iter().any(|&side| side != 0);
            let mut material_coverage = Vec::new();
            material_coverage
                .try_reserve_exact(definitions.len())
                .map_err(scene_cache::Error::from)?;
            material_coverage.extend(definitions.iter().map(materials::tests_coverage));
            let mut light_areas = Vec::new();
            light_areas
                .try_reserve_exact(source.light_areas.len())
                .map_err(scene_cache::Error::from)?;
            light_areas.extend_from_slice(source.light_areas);
            let extent = [source.extent[0], source.extent[1], 1];
            let atlas = Image::new(&mut self.context, extent, ImageFormat::Rgba8Srgb)?;
            atlas.upload(&mut self.context, source.albedo)?;
            let mers = Image::new(&mut self.context, extent, ImageFormat::Rgba8Unorm)?;
            mers.upload(&mut self.context, source.mers)?;
            let auxiliary = Image::new(&mut self.context, extent, ImageFormat::Rgba8Unorm)?;
            auxiliary.upload(&mut self.context, source.auxiliary)?;
            let alias_bytes = bytemuck::cast_slice(source.emission_alias_words);
            let resources = MaterialGpu {
                table: self.material_gpu.table,
                mers,
                auxiliary,
                emission_alias: Buffer::upload(
                    &mut self.context,
                    if alias_bytes.is_empty() {
                        &[0; 16]
                    } else {
                        alias_bytes
                    },
                )?,
            };
            let (resources, generation) =
                self.prepare_material_generation(atlas, resources, &flows, definitions)?;
            let mut candidates = generation.regular.roots().to_vec();
            if let Some(program) = &generation.reordered {
                candidates.extend(program.roots());
            }
            self.collect_with(&candidates)?;
            // 此后只移动已经准备的所有者；不得以清理失败向宿主报告“保留旧世代”。
            self.renderer = None;
            if let Some(view) = &mut self.view {
                view.progress.reset();
            }
            history::record_global(GlobalReset::SceneDomainReplaced, self.rr_previous.is_some());
            self.rr_previous = None;
            self.atlas = atlas;
            self.atlas_colors = colors;
            self.material_gpu = resources;
            self.material_coverage = material_coverage;
            self.light_sides = light_sides;
            self.has_emission = has_emission;
            self.region_cache = region_cache;
            self.attribute_revisions.clear();
            self.scene.top = self.program_scene.top;
            self.scene.instance_quad_bases = self.program_scene.instance_quad_bases;
            self.lighting = lighting;
            self.lighting_gpu = self.program_lighting;
            self.light_areas = light_areas;
            self.map_ranges = ranges;
            self.resource_generation = source.generation;
            self.generation = Some(generation);
            self.lighting_dirty = true;
            Ok(())
        })();
        if result.is_err() {
            self.collect_resources()?;
        } else {
            self.context.flush_pipeline_cache();
        }
        result
    }

    fn prepare_material_generation(
        &mut self,
        atlas: Image,
        mut resources: MaterialGpu,
        flows: &[openpbr::FlowKey],
        definitions: &[MaterialDefinition],
    ) -> Result<(MaterialGpu, MaterialGeneration), Error> {
        if flows.len() != definitions.len() {
            return Err(Error::Invalid("材质Flow与输入定义数量不匹配"));
        }
        let hit = materials::hit_features(definitions);
        let coverage = format!("-DPRIMEPT_COVERAGE_MASK={}", hit.coverage);
        let lights = if flows.iter().any(|flow| flow.emits()) {
            "-DPRIMEPT_LIGHT_MASK=3"
        } else {
            "-DPRIMEPT_LIGHT_MASK=1"
        };
        let programs = openpbr::program::ProgramSet::compile_variants(
            &self.compiler,
            flows
                .iter()
                .copied()
                .zip(definitions)
                .map(|(flow, definition)| (flow, materials::variant(flow, definition))),
            openpbr::program::Template {
                source: self
                    .shader_source
                    .to_str()
                    .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                entry: "shade_opaque",
                output: "flow.spv",
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &["-fp-mode", "precise", lights, &coverage],
            },
        )?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(definitions.len())
            .map_err(scene_cache::Error::from)?;
        for (definition, &slot) in definitions.iter().zip(programs.plan().bindings()) {
            if slot > u32::MAX >> 2 {
                return Err(Error::Invalid("材质callable索引超过30bit"));
            }
            let mut words = definition.words();
            // 受控GPU派生记录：OpenPBR flags在低2bit，资源计划的SBT槽在其余bit。
            words[4][0] |= slot << 2;
            rows.push(words);
        }
        resources.table = Buffer::upload(&mut self.context, bytemuck::cast_slice(&rows))?;
        let descriptors = self.renderer_bindings(
            &self.program_view,
            &atlas,
            &self.program_scene,
            resources,
            self.program_lighting,
        );
        let (trace, post) = self.allocate_bindings(descriptors)?;
        let same_code = self
            .generation
            .as_ref()
            .is_some_and(|previous| hit == previous.hit && programs.same_code(&previous.programs));
        let callables: Vec<_> = programs
            .artifacts()
            .iter()
            .map(|artifact| RayShader {
                spirv: artifact.bytes(),
                entry: c"shade_opaque",
            })
            .collect();
        let regular = if same_code {
            self.generation.as_ref().unwrap().regular.rebind(
                &mut self.context,
                trace,
                post,
                [1, 1],
            )?
        } else {
            Renderer::new(
                &mut self.context,
                self::programs(&callables, false, hit),
                trace,
                post,
                [1, 1],
            )?
        };
        let reordered = if self.context.capabilities().invocation_reorder {
            Some(
                if same_code
                    && let Some(previous) =
                        self.generation.as_ref().and_then(|g| g.reordered.as_ref())
                {
                    previous.rebind(&mut self.context, trace, post, [1, 1])?
                } else {
                    Renderer::new(
                        &mut self.context,
                        self::programs(&callables, true, hit),
                        trace,
                        post,
                        [1, 1],
                    )?
                },
            )
        } else {
            None
        };
        Ok((
            resources,
            MaterialGeneration {
                programs,
                hit,
                regular,
                reordered,
            },
        ))
    }

    /// 设置宿主薄捕获确认的相机初始介质(IOR,RGB sigma/m)；非有限或负消光拒绝。
    /// 相机进入介质只重启原始PT样本，保留RR时间历史。
    pub fn set_initial_medium(&mut self, medium: [f32; 4]) -> Result<(), Error> {
        if medium.iter().any(|v| !v.is_finite())
            || !(1. ..=4.).contains(&medium[0])
            || medium[1..].iter().any(|&v| v < 0.)
        {
            return Err(Error::Invalid("相机初始介质无效"));
        }
        if self.initial_medium != medium {
            self.initial_medium = medium;
            if let Some(view) = &mut self.view {
                view.progress.reset();
            }
        }
        Ok(())
    }

    /// 材质域更新后，即使管理器没有几何patch，也须重新准备发光分布。
    pub const fn scene_resources_dirty(&self) -> bool {
        self.lighting_dirty
    }

    #[cfg(test)]
    /// 无窗口测试的单mesh入口；生产通过region视图发布。
    pub fn set_scene(&mut self, mesh: &Mesh) -> Result<(), Error> {
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
        self.region_cache = scene_cache::Cache::new()?;
        self.attribute_revisions.clear();
        Ok(())
    }

    /// 发布已排序、唯一且完整的region借用视图；输入只在调用内有效。
    ///
    /// 宿主须已确认旧命令完成。几何revision决定BLAS，content revision决定属性写入；
    /// 属性buffer容量保留并仅写变化范围，只有增长才重写全部活动范围。
    /// 管理器空发布不触碰GPU；anchor只改变TLAS。准备或映射失败保留旧目录和属性。
    pub fn set_regions(&mut self, source: prime_scene::RenderView<'_>) -> Result<(), Error> {
        self.scene_update = None;
        self.complete_previous()?;
        let result = self.set_regions_inner(source);
        if result.is_ok() && self.scene_update.is_some_and(|timing| !timing.changed) {
            return result;
        }
        if result.is_err() {
            self.collect_resources()?;
        }
        result
    }

    /// 最近一次成功场景调用的墙钟及工作集；失败时为None，空发布保存零工作量。
    pub const fn scene_timing(&self) -> Option<SceneUpdateTiming> {
        self.scene_update
    }

    fn set_regions_inner(&mut self, source: prime_scene::RenderView<'_>) -> Result<(), Error> {
        if source.generation != self.resource_generation {
            return Err(Error::Invalid("region与材质资源世代不匹配"));
        }
        if source.regions.len() > 1 << 24 {
            return Err(Error::Invalid("region实例数量超过24位身份域"));
        }
        if source.anchor.iter().any(|v| !v.is_finite())
            || source.regions.windows(2).any(|p| p[0].key >= p[1].key)
            || source
                .regions
                .iter()
                .any(|r| r.geometry_revision == 0 || r.content_revision == 0 || r.mesh.is_empty())
        {
            return Err(Error::Invalid("region视图须有序唯一、非空且revision非零"));
        }
        let mut metrics = SceneUpdateTiming {
            instances: source.regions.len(),
            ..Default::default()
        };
        let same_geometry = source.regions.len() == self.region_cache.entries().len()
            && source
                .regions
                .iter()
                .zip(self.region_cache.entries())
                .all(|(r, old)| r.key == old.key && r.geometry_revision == old.revision);
        let same_content = source
            .regions
            .iter()
            .map(|r| (r.key, r.content_revision))
            .eq(self.attribute_revisions.iter().copied());
        if same_geometry && same_content && source.anchor == self.anchor && !self.lighting_dirty {
            self.scene_update = Some(metrics);
            return Ok(());
        }
        let mut matches = region_match::regions(
            source.regions,
            self.region_cache.entries(),
            &self.attribute_revisions,
        )?;
        // 同一几何版本可以更换材质。颜色补丁不影响覆盖，其余属性更新只有在旧 BLAS
        // 跳过覆盖时才需要检查是否仍可复用。非 opaque 的旧 BLAS 对所有材质都正确。
        for (region, matched) in source.regions.iter().zip(&mut matches) {
            if matched.attributes_dirty
                && !region.mesh.attribute_only()
                && matched
                    .reuse
                    .is_some_and(|i| self.region_cache.entries()[i].opaque)
                && region.mesh.quads().any(|quad| {
                    self.material_coverage
                        .get(quad.material_id().index() as usize)
                        .is_none_or(|&coverage| coverage)
                })
            {
                matched.reuse = None;
            }
        }
        let mut update = self.region_cache.begin(source.regions.len())?;
        let (slots, entries, _) = update.parts_mut();
        let started = std::time::Instant::now();
        let dirty_triangles =
            source
                .regions
                .iter()
                .zip(&matches)
                .try_fold(0usize, |count, (region, matched)| {
                    if matched.reuse.is_some() {
                        Ok(count)
                    } else {
                        count
                            .checked_add(
                                region
                                    .mesh
                                    .len()
                                    .checked_mul(2)
                                    .ok_or(Error::Invalid("dirty三角数量溢出"))?,
                            )
                            .ok_or(Error::Invalid("dirty三角数量溢出"))
                    }
                })?;
        let mut triangles = Vec::new();
        triangles
            .try_reserve_exact(dirty_triangles)
            .map_err(scene_cache::Error::from)?;
        let mut pending = Vec::with_capacity(source.regions.len());
        for (region, matched) in source.regions.iter().zip(&matches) {
            if let Some(index) = matched.reuse {
                entries.push(scene_cache::Prepared::Reused(index));
                continue;
            }
            let count = u32::try_from(region.mesh.len())
                .map_err(|_| Error::Invalid("region面索引超过u32"))?;
            let range = slots.take(count)?;
            if range.end > self.attribute_slot_limit {
                return Err(Error::Invalid("region属性槽超过设备缓冲容量"));
            }
            let start = triangles.len();
            let mut opaque = true;
            for quad in region.mesh.quads() {
                let coverage = self
                    .material_coverage
                    .get(quad.material_id().index() as usize)
                    .ok_or(Error::Invalid("构建AS前quad记录或材质身份无效"))?;
                if !quad.valid() {
                    return Err(Error::Invalid("构建AS前quad记录或材质身份无效"));
                }
                opaque &= !coverage;
                let [a, b, c, d] = quad.positions();
                triangles.extend([[a, b, c], [a, c, d]]);
            }
            pending.push((entries.len(), start..triangles.len()));
            entries.push(scene_cache::Prepared::Built(RegionGpu {
                key: region.key,
                revision: region.geometry_revision,
                range,
                blas: None,
                opaque,
            }));
        }
        let same_acceleration = same_geometry && pending.is_empty();
        metrics.dirty_regions = pending.len();
        let inputs: Vec<_> = pending
            .iter()
            .map(|(index, range)| vulkan::TriangleBlasInput {
                triangles: &triangles[range.clone()],
                opaque: entries[*index].get(self.region_cache.entries()).opaque,
                opacity: None,
            })
            .collect();
        metrics.geometry_ns = elapsed_ns(started);
        metrics.triangle_bytes = size_of_val(triangles.as_slice()) as u64;
        metrics.triangle_capacity_bytes =
            (triangles.capacity() * size_of::<[[f32; 3]; 3]>()) as u64;
        metrics.pending_capacity_bytes =
            (pending.capacity() * size_of::<(usize, std::ops::Range<usize>)>()) as u64;
        metrics.input_capacity_bytes =
            (inputs.capacity() * size_of::<vulkan::TriangleBlasInput<'_>>()) as u64;
        let top = if !same_acceleration || source.anchor != self.anchor {
            let packing = std::time::Instant::now();
            let mut instances = Vec::new();
            instances
                .try_reserve_exact(entries.len())
                .map_err(scene_cache::Error::from)?;
            let mut next_blas = 0;
            for (index, entry) in entries.iter().enumerate() {
                let built = matches!(entry, scene_cache::Prepared::Built(_));
                let entry = entry.get(self.region_cache.entries());
                let geometry = if built {
                    let geometry = vulkan::BatchBlas::New(next_blas);
                    next_blas += 1;
                    geometry
                } else {
                    vulkan::BatchBlas::Existing(
                        entry.blas.ok_or(Error::Invalid("未完成region BLAS"))?,
                    )
                };
                let offset: [f32; 3] = std::array::from_fn(|axis| {
                    (f64::from(entry.key[axis]) * 64.0 - source.anchor[axis]) as f32
                });
                instances.push(TriangleInstance {
                    geometry,
                    transform: [
                        [1., 0., 0., offset[0]],
                        [0., 1., 0., offset[1]],
                        [0., 0., 1., offset[2]],
                    ],
                    custom_index: index as u32,
                });
            }
            metrics.instance_capacity_bytes =
                (instances.capacity() * size_of::<TriangleInstance<vulkan::BatchBlas>>()) as u64;
            metrics.attribute_pack_ns = elapsed_ns(packing);
            let accelerating = std::time::Instant::now();
            let batch = self.context.triangle_scene_batch(&inputs, &instances)?;
            metrics.acceleration_ns = elapsed_ns(accelerating);
            metrics.tlas_ns = batch.tlas_prepare_ns;
            metrics.as_submit_wait_ns = batch.blas.timing.submit_wait_ns;
            if !inputs.is_empty() {
                metrics.batch = batch.blas.timing;
            }
            for ((index, _), blas) in pending.iter().zip(batch.blas.blases) {
                let scene_cache::Prepared::Built(entry) = &mut entries[*index] else {
                    unreachable!("dirty输入对应新记录");
                };
                entry.blas = Some(blas);
            }
            batch.top
        } else {
            self.scene.top
        };
        drop(inputs);
        drop(triangles);
        drop(pending);
        let packing = std::time::Instant::now();
        let end = entries
            .iter()
            .map(|e| e.get(self.region_cache.entries()).range.end)
            .max()
            .unwrap_or(0);
        let grow = u64::from(end) * 48 > self.scene.quads.len();
        let mut attributes = Vec::new();
        let dirty_count = source
            .regions
            .iter()
            .zip(entries.iter())
            .zip(&matches)
            .filter(|((_, e), matched)| {
                matches!(e, scene_cache::Prepared::Built(_)) || matched.attributes_dirty
            })
            .try_fold(0usize, |n, ((r, e), _)| {
                let partial =
                    !matches!(e, scene_cache::Prepared::Built(_)) && r.mesh.attribute_only();
                let count = if partial {
                    r.mesh
                        .attribute_ranges()
                        .map(|range| range.quads.len())
                        .sum()
                } else {
                    r.mesh.len()
                };
                n.checked_add(count).ok_or(Error::Invalid("属性数量溢出"))
            })?;
        attributes
            .try_reserve_exact(dirty_count)
            .map_err(scene_cache::Error::from)?;
        let mut ranges = Vec::with_capacity(source.regions.len());
        let mut versions = Vec::with_capacity(source.regions.len());
        for ((region, entry), matched) in source.regions.iter().zip(entries.iter()).zip(&matches) {
            let built = matches!(entry, scene_cache::Prepared::Built(_));
            let entry = entry.get(self.region_cache.entries());
            let dirty = built || matched.attributes_dirty;
            if dirty {
                if !built && region.mesh.attribute_only() {
                    for patch in region.mesh.attribute_ranges() {
                        let start = attributes.len();
                        attributes.extend(patch.attributes());
                        ranges.push((
                            (entry.range.start as usize + patch.first) * 48,
                            start..attributes.len(),
                        ));
                    }
                } else {
                    let start = attributes.len();
                    // 新几何已在AS输入边界校验；复用几何的普通属性候选仍需检查。
                    if !built {
                        for quad in region.mesh.quads() {
                            if !quad.valid()
                                || quad.material_id().index() as usize >= self.light_sides.len()
                            {
                                return Err(Error::Invalid("quad记录或材质身份不属于当前资源世代"));
                            }
                        }
                    }
                    attributes.extend(region.mesh.attributes());
                    ranges.push((entry.range.start as usize * 48, start..attributes.len()));
                }
            }
            versions.push((region.key, region.content_revision));
        }
        metrics.attribute_capacity_bytes =
            (attributes.capacity() * size_of::<[[u32; 4]; 3]>()) as u64;
        metrics.attribute_pack_ns += elapsed_ns(packing);
        metrics.geometry_ns += metrics.attribute_pack_ns;
        let light_started = std::time::Instant::now();
        let bases: Vec<_> = entries
            .iter()
            .map(|e| e.get(self.region_cache.entries()).range.start)
            .collect();
        let lights = self.lighting.prepare(
            source,
            &bases,
            &self.light_sides,
            &self.light_areas,
            self.has_emission,
            self.resource_generation,
        )?;
        metrics.light_pack_ns = elapsed_ns(light_started);
        metrics.geometry_ns += metrics.light_pack_ns;
        let light_started = std::time::Instant::now();
        let lighting_gpu = if let Some(tables) = &lights.tables {
            LightingGpu::upload(&mut self.context, Some(tables))?
        } else {
            self.lighting_gpu
        };
        if lights.tables.is_some() {
            metrics.light_upload_ns = elapsed_ns(light_started);
            metrics.light_upload_bytes = lighting_gpu.alias.len()
                + lighting_gpu.meta.len()
                + lighting_gpu.vertices.len()
                + lighting_gpu.inverse.len()
                + lighting_gpu.regions.len();
            metrics.resources_ns += metrics.light_upload_ns;
        }
        let writes: Vec<_> = ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&attributes[range.clone()])))
            .collect();
        let uploading = std::time::Instant::now();
        let quads = if grow {
            let capacity = end
                .max(4096)
                .checked_next_power_of_two()
                .ok_or(Error::Invalid("属性容量溢出"))?;
            let buffer = Buffer::mapped_device_local(&mut self.context, u64::from(capacity) * 48)?;
            // 旧槽只复制需继承的前缀，完整替换的槽由后续属性写入覆盖。
            let retained_end = source
                .regions
                .iter()
                .zip(entries.iter())
                .zip(&matches)
                .filter(|((region, entry), matched)| {
                    !matches!(entry, scene_cache::Prepared::Built(_))
                        && (!matched.attributes_dirty || region.mesh.attribute_only())
                })
                .map(|((_, entry), _)| entry.get(self.region_cache.entries()).range.end)
                .max()
                .unwrap_or(0);
            if retained_end != 0 {
                metrics.attribute_copy_bytes = u64::from(retained_end) * 48;
                self.scene.quads.copy_prefix_to(
                    &mut self.context,
                    &buffer,
                    metrics.attribute_copy_bytes,
                )?;
            }
            buffer
        } else {
            self.scene.quads
        };
        let mut allocation_ns = if grow { elapsed_ns(uploading) } else { 0 };
        // 实例身份只引用同序目录；属性槽可独立扩展。anchor变化和纯属性更新复用目录。
        let base_write = (!same_acceleration && !bases.is_empty())
            .then(|| (0, bytemuck::cast_slice(bases.as_slice())));
        let instance_quad_bases = if let Some((_, bytes)) = base_write
            && (bytes.len() as u64 > self.scene.instance_quad_bases.len()
                || self.scene.instance_quad_bases == self.program_scene.instance_quad_bases)
        {
            let started = std::time::Instant::now();
            let capacity = bytes.len().max(256).next_power_of_two();
            let buffer = Buffer::mapped_device_local(&mut self.context, capacity as u64)?;
            allocation_ns += elapsed_ns(started);
            buffer
        } else {
            self.scene.instance_quad_bases
        };
        let bindings_changed = top != self.scene.top
            || quads != self.scene.quads
            || instance_quad_bases != self.scene.instance_quad_bases
            || lighting_gpu != self.lighting_gpu;
        if bindings_changed {
            let collecting = std::time::Instant::now();
            let mut candidates = lighting_gpu.roots().to_vec();
            candidates.extend([
                vulkan::native::Resource::from(top),
                quads.into(),
                instance_quad_bases.into(),
            ]);
            self.collect_with(&candidates)?;
            metrics.collect_ns = elapsed_ns(collecting);
        }
        metrics.resources_ns += metrics.collect_ns;
        // 目录独占校验之后只有一次原子映射写入；成功复制后发布阶段不再失败。
        let publication = self
            .region_cache
            .prepare(update, |entry| entry.range.clone())?;
        let light_publication = self.lighting.publication(lights)?;
        let patches = light_publication.patches();
        fn rows(values: &[(usize, [u32; 4])]) -> Vec<(usize, &[u8])> {
            values
                .iter()
                .map(|(row, value)| (row * 16, bytemuck::bytes_of(value)))
                .collect()
        }
        let alias_writes = rows(&patches.alias);
        let meta_writes = rows(&patches.meta);
        let inverse_writes = rows(&patches.inverse);
        let region_writes = rows(&patches.regions);
        let vertex_writes: Vec<_> = patches
            .vertices
            .iter()
            .map(|(row, value)| (row * 16, bytemuck::bytes_of(value)))
            .collect();
        let light_patch_bytes = (alias_writes.len()
            + meta_writes.len()
            + inverse_writes.len()
            + region_writes.len()
            + vertex_writes.len()) as u64
            * 16;

        let writes_attributes = !writes.is_empty() || base_write.is_some();
        if writes_attributes || light_patch_bytes != 0 {
            let writing = std::time::Instant::now();
            Buffer::write_scattered_batches(
                &mut self.context,
                &[
                    (&quads, &writes),
                    (&instance_quad_bases, base_write.as_slice()),
                    (&lighting_gpu.alias, &alias_writes),
                    (&lighting_gpu.meta, &meta_writes),
                    (&lighting_gpu.vertices, &vertex_writes),
                    (&lighting_gpu.inverse, &inverse_writes),
                    (&lighting_gpu.regions, &region_writes),
                ],
            )?;
            // 共同映射/复制的墙钟只归因一次，不能按buffer把同一等待重复累加。
            let wall = elapsed_ns(writing);
            if !writes_attributes {
                metrics.light_upload_ns += wall;
            } else {
                metrics.attribute_upload_ns = wall;
            }
            metrics.light_upload_bytes += light_patch_bytes;
            if !writes_attributes {
                metrics.resources_ns += wall;
            }
        }
        metrics.attribute_upload_ns += allocation_ns;
        metrics.attribute_upload_bytes =
            (attributes.len() * 48 + base_write.map_or(0, |(_, bytes)| bytes.len())) as u64;
        metrics.resources_ns += metrics.attribute_upload_ns;
        let publishing = std::time::Instant::now();
        publication.commit();
        light_publication.commit();
        self.lighting.set_epoch(self.resource_generation);
        self.lighting_gpu = lighting_gpu;
        self.lighting_dirty = false;
        self.scene = Scene {
            top,
            quads,
            instance_quad_bases,
        };
        self.anchor = source.anchor;
        self.attribute_revisions = versions;
        if bindings_changed {
            self.renderer = None;
        }
        if let Some(view) = &mut self.view {
            view.progress.reset();
        }
        metrics.publish_ns = elapsed_ns(publishing);
        metrics.resources_ns += metrics.publish_ns;
        metrics.changed = true;
        self.scene_update = Some(metrics);
        Ok(())
    }

    #[cfg(test)]
    fn set_sections(&mut self, source: &prime_world::Scene) -> Result<(), Error> {
        let regions: Vec<_> = source.render_regions().collect();
        self.set_regions(prime_scene::RenderView {
            generation: source.generation(),
            regions: &regions,
            anchor: source.anchor(),
        })
    }

    // 单场景和两个视图的固定根；录制命令另由Context保护短命绑定。
    fn collect_resources(&mut self) -> Result<(), Error> {
        self.collect_with(&[])
    }

    fn collect_with(&mut self, extra: &[vulkan::native::Resource]) -> Result<(), Error> {
        let mut roots = vec![
            self.scene.top.into(),
            self.scene.quads.into(),
            self.scene.instance_quad_bases.into(),
            self.program_scene.top.into(),
            self.program_scene.quads.into(),
            self.program_scene.instance_quad_bases.into(),
            self.atlas.into(),
            self.energy.into(),
            self.nearest.into(),
            self.linear.into(),
        ];
        roots.extend([
            vulkan::native::Resource::from(self.material_gpu.table),
            self.material_gpu.mers.into(),
            self.material_gpu.auxiliary.into(),
        ]);
        roots.push(self.material_gpu.emission_alias.into());
        roots.extend(self.lighting_gpu.roots());
        roots.extend(self.program_lighting.roots());
        roots.extend(self.atmosphere.roots());
        roots.extend(self.program_view.roots());
        if let Some(view) = &self.view {
            roots.extend(view.roots());
        }
        if let Some(generation) = &self.generation {
            roots.extend(generation.regular.roots());
            if let Some(program) = &generation.reordered {
                roots.extend(program.roots());
            }
        }
        if let Some(renderer) = &self.renderer {
            roots.extend(renderer.roots());
        }
        #[cfg(test)]
        roots.extend(self.test_roots.iter().copied());
        roots.extend_from_slice(extra);
        self.context.collect(&roots)?;
        Ok(())
    }

    fn renderer_bindings(
        &self,
        view: &View,
        atlas: &Image,
        scene: &Scene,
        materials: MaterialGpu,
        lighting: LightingGpu,
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
        trace.extend(view.landing[..5].iter().map(buffer));
        trace.push(buffer(&view.prefix));
        trace.extend([
            buffer(self.atmosphere.bindings().sky_mapping),
            Binding::SampledImage(*self.atmosphere.bindings().sky),
            Binding::SampledImage(*self.atmosphere.bindings().optical_depth),
        ]);
        trace.extend([1, 2, 3, 4, 5, 7, 8].map(|i| Binding::StorageImage(view.rr.images[i])));
        trace.extend([
            buffer(&view.landing[5]),
            buffer(&materials.table),
            Binding::SampledImage(materials.mers),
            Binding::SampledImage(materials.auxiliary),
            buffer(&lighting.alias),
            buffer(&lighting.meta),
            buffer(&lighting.vertices),
            buffer(&lighting.inverse),
            buffer(&lighting.regions),
            buffer(&materials.emission_alias),
            buffer(&scene.instance_quad_bases),
        ]);
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
        let descriptors = self.renderer_bindings(
            view,
            &self.atlas,
            &self.scene,
            self.material_gpu,
            self.lighting_gpu,
        );
        let (trace, post) = self.allocate_bindings(descriptors)?;
        let generation = self.generation.as_ref().unwrap();
        let program = if self.invocation_reorder {
            generation
                .reordered
                .as_ref()
                .expect("资源准备已检查SER能力")
        } else {
            &generation.regular
        };
        Ok(program.rebind(&mut self.context, trace, post, extent)?)
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
            let sky_enabled = environment.enabled();
            let solar_scale = if sky_enabled {
                self.solar_reference_scale * self.sun_gain
            } else {
                0.0
            };
            let block_emission_scale = if self.has_emission {
                self.block_emission_gain
            } else {
                1.0
            };
            if sky_enabled {
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
                let storage_event = match &self.view {
                    None => history::StorageEvent::Initial,
                    Some(view) if view.output_extent != extent => {
                        history::StorageEvent::ExtentChanged
                    }
                    Some(view) if view.rr_enabled != rr_enabled => {
                        history::StorageEvent::ReconstructionChanged
                    }
                    Some(view) if rr_enabled && view.quality != self.rr_quality => {
                        history::StorageEvent::QualityChanged
                    }
                    Some(_) => history::StorageEvent::RuntimeRecreated,
                };
                let history_valid = self.rr_previous.is_some();
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
                    Some(
                        self.view
                            .as_ref()
                            .map_or(&self.program_view.display, |view| &view.display),
                    ),
                    Some(&self.program_view),
                )?);
                history::record_storage(storage_event, history_valid);
                self.rr_previous = None;
                self.renderer = None;
            }
            if self.renderer.is_none() {
                self.renderer = Some(self.create_renderer()?);
            }
            let current = CameraFrame::new(camera, self.anchor)?;
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
                        solar_scale.to_bits(),
                        block_emission_scale.to_bits(),
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
                self.display_drt
                    .with_exposure(environment.exposure_ev.exp2())?
                    .to_array()
                    .map(f32::to_bits),
            );
            words.extend(environment.sun_direction.map(f32::to_bits));
            words.push(if sky_enabled { 1.0f32.to_bits() } else { 0 });
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
            words.extend([self.rr_sample, block_emission_scale.to_bits(), 0, 0]);
            words.extend(
                self.rr_previous
                    .as_ref()
                    .unwrap_or(&current)
                    .camera_words(self.anchor)?
                    .map(f32::to_bits),
            );
            words.extend([view.output_extent[0], view.output_extent[1], 0, 0]);
            words.extend(self.anchor.map(|v| ((v / 64.) as i32) as u32));
            words.push(0);
            words.extend(self.initial_medium.map(f32::to_bits));
            view.params
                .write(&mut self.context, 0, bytemuck::cast_slice(&words))?;
            if sky_enabled && let Some(recorded) = self.atmosphere.record(&mut self.context)? {
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
    pub fn draw(
        &mut self,
        command: u64,
        image: u64,
        view: u64,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        options: Options,
    ) -> Result<(), Error> {
        if self.generation.is_none() {
            return Err(Error::Invalid("draw前需要完整资源准备"));
        }
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
                let current = CameraFrame::new(camera, self.anchor)?;
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
                )?;
                let evaluated = match unsafe { self.rr_runtime.as_mut().unwrap().evaluate(&frame) }
                {
                    Ok(()) => true,
                    Err(error) if error.unsafe_recording() => return Err(error.source.into()),
                    Err(error) => {
                        eprintln!("RR 本帧回退：{error}");
                        history::record_recovery(self.rr_previous.is_some());
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

fn validate_aliases(materials: &[MaterialDefinition], words: &[u32]) -> Result<(), Error> {
    let mut visited = std::collections::BTreeSet::new();
    for material in materials {
        let offset = (material.control[3] >> 1) as usize;
        if offset == 0 || !visited.insert(offset) {
            continue;
        }
        let h = words
            .get(
                offset
                    ..offset
                        .checked_add(8)
                        .ok_or(Error::Invalid("alias offset溢出"))?,
            )
            .ok_or(Error::Invalid("alias header越界"))?;
        let columns = h[2].checked_add(1).ok_or(Error::Invalid("alias列数溢出"))?;
        if h[0] == 0
            || h[1] == 0
            || u64::from(h[0]) * u64::from(h[1]) > 65536
            || !columns.is_power_of_two()
            || columns > 1 << 24
            || h[3] != 24 - columns.trailing_zeros()
            || h[7] == 0
            || h[7] > columns
        {
            return Err(Error::Invalid("alias header域无效"));
        }
        let grid_end = u64::from(h[4]) + u64::from(h[0]) * u64::from(h[1]);
        let entry_end = u64::from(h[5]) + u64::from(columns) * 8;
        let remaining = (words.len() - offset) as u64;
        if h[4] < 8
            || u64::from(h[5]) < grid_end
            || u64::from(h[6]) < entry_end
            || u64::from(h[6]) > remaining
        {
            return Err(Error::Invalid("alias表范围无效"));
        }
        for &cell in &words[offset + h[4] as usize..offset + grid_end as usize] {
            if cell != u32::MAX && cell >= h[7] {
                return Err(Error::Invalid("alias反查索引无效"));
            }
        }
        let capacity = 1u32 << h[3];
        let mut mass = 0u64;
        for i in 0..columns as usize {
            let at = offset + h[5] as usize + i * 8;
            let e = &words[at..at + 8];
            if e[0] > capacity || e[1] >= h[7] || (i >= h[7] as usize && (e[0] != 0 || e[2] != 0)) {
                return Err(Error::Invalid("alias列选择域无效"));
            }
            mass += u64::from(e[2]);
        }
        if mass != u64::from(light_sampling::SELECTORS) {
            return Err(Error::Invalid("alias整数总质量无效"));
        }
        for i in 0..h[7] as usize {
            let at = offset + h[5] as usize + i * 8;
            let e = &words[at..at + 8];
            let patch_end = u64::from(h[6]) + (u64::from(e[3]) + u64::from(e[4])) * 8;
            if e[4] == 0
                || patch_end > remaining
                || !f32::from_bits(e[5]).is_finite()
                || f32::from_bits(e[5]) <= 0.
            {
                return Err(Error::Invalid("alias patch范围或密度无效"));
            }
            for patch in 0..e[4] as usize {
                let at = offset + h[6] as usize + (e[3] as usize + patch) * 8;
                let p = &words[at..at + 8];
                if p[0] > 1
                    || !f32::from_bits(p[1]).is_finite()
                    || f32::from_bits(p[1]) <= 0.
                    || p[2..].iter().any(|&v| !f32::from_bits(v).is_finite())
                {
                    return Err(Error::Invalid("alias patch几何域无效"));
                }
            }
        }
    }
    Ok(())
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
