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
//! 完整HG照明与RR导引独立准备；启用RR不改变照明调用图，导引每帧只执行一次。
//! region 增量更新复用已准备的材质资源，旧 GPU 资源在最后一次实际使用完成后退役。
//! 局部几何变动按 region 重建 BLAS，纯 tint 走属性更新，anchor 变化更新 TLAS 平移。
//!
//! 普通PT的完整Hit Group/专用导引要求设备已启用`VK_EXT_ray_tracing_invocation_reorder`；
//! 是否实际执行SER由设置选择。未启用该能力时保留Flow callable兼容路径，其作者法线预解码
//! 与旧导引仍是通用实现，不属于完整Hit Group路径的逐阶段材质裁剪保证。实时PT先缓存首视线，再独立运行delta导引
//! 和随机输运；Guide与输运从同一首面重启，不共用后缀。离线仍追踪完整路径且不生成导引。
//! 实时1／2样本与离线采用独立入口及最小链接集；有限film、合成和显示程序在准备阶段建立，
//! FramePlan统一选择录制与参数，帧内只重绑和选择已有程序。

use crate::scene::{Camera, Environment, Mesh};
use prime_scene::MaterialDefinition;
use simple_engine::{
    atmosphere, camera, light_sampling, openpbr, pt, scene_cache, shader_build, vulkan,
};
#[path = "attribute_update.rs"]
mod attribute_update;
#[path = "lighting.rs"]
mod lighting;
#[path = "materials.rs"]
mod materials;
#[path = "opacity.rs"]
mod opacity;
#[path = "region_match.rs"]
mod region_match;
#[path = "scene_upload.rs"]
mod scene_upload;
#[path = "terrain_packets.rs"]
mod terrain_packets;
#[path = "terrain_production.rs"]
mod terrain_production;
use terrain_production::{CpuPackets, FragmentSpan};

#[path = "known_geometry.rs"]
mod known_geometry;

use crate::{reconstruction::CameraFrame, streamline};
use std::path::{Path, PathBuf};
#[path = "exposure.rs"]
mod exposure;
#[path = "history.rs"]
mod history;
pub use history::GlobalReset;
use simple_engine::pt::gpu::{Programs, RealtimeFrame, RealtimePrograms, Renderer};
#[path = "frame_plan.rs"]
mod frame_plan;
use frame_plan::{Composite, FramePlan};
use simple_engine::vulkan::{
    AccelerationStructure, TriangleInstance,
    interop::RayShader,
    native::{
        Binding, Bindings, Buffer, Capabilities, Commands, ComputePipeline, Context,
        ExternalBinding, ExternalKind, Flight, Image, ImageFormat, RawDevice, RayTracingPipeline,
        Recorded, Sampler,
    },
};

/// 渲染输入、资源、程序或SDK失败；保留实际底层来源。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 后台地形生产者创建或交接失败。
    #[error(transparent)]
    Production(#[from] scene_cache::ProducerError),
    /// 并行工作池的系统创建错误。
    #[error(transparent)]
    TerrainWorkers(#[from] rayon::ThreadPoolBuildError),
    /// GPU 故障进度无法写入宿主标准错误；保留原始 IO 失败。
    #[error("写入 GPU 故障诊断失败")]
    DiagnosticOutput(#[source] std::io::Error),
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
    /// 精确二值 OMM 输入或内存申请失败。
    #[error(transparent)]
    Opacity(#[from] simple_engine::opacity::Error),
    /// 光源选择或面积输入无效。
    #[error(transparent)]
    Lights(#[from] light_sampling::Error),
    /// 活动光源目录准备失败。
    #[error(transparent)]
    LightRegistry(#[from] light_sampling::registry::Error),
    /// 光源BVH输入或构建失败。
    #[error(transparent)]
    LightBvh(#[from] simple_engine::light_sampling::bvh::Error),
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
    tokens: Buffer,
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
    /// 完整资源期实际 cutout UV；未列出的几何保持软件覆盖，不运行期懒准备。
    pub opacity_quads: &'a [simple_engine::opacity::Quad],
}

#[path = "capture.rs"]
pub mod capture;

#[derive(Clone, Copy)]
struct MaterialGpu {
    table: Buffer,
    mers: Image,
    auxiliary: Image,
    emission_alias: Buffer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LightingGpu {
    tree: Buffer,
    alias: Buffer,
    meta: Buffer,
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
            tree: Buffer::new(context, 32, false)?,
            alias: upload(
                context,
                tables.map_or(&[0u8; 16], |t| bytemuck::cast_slice(&t.selection)),
            )?,
            meta: upload(
                context,
                tables.map_or(&[], |t| bytemuck::cast_slice(&t.meta)),
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
            self.tree.into(),
            self.alias.into(),
            self.meta.into(),
            self.inverse.into(),
            self.regions.into(),
        ]
    }
}

struct PackedRegions {
    patches: std::collections::BTreeMap<usize, terrain_packets::AttributeRows>,
    update: scene_cache::Update<RegionGpu>,
    pending: Vec<(usize, std::ops::Range<usize>)>,
    opacity_indices: Vec<u32>,
}
// 整个候选只生成一次的 CPU 写入和光源事务；设备列在完成前作为 collect 的根。
struct Transfer {
    upload: vulkan::ColumnUpload,
    attributes: Vec<[[u32; 4]; 3]>,
    ranges: Vec<(usize, std::ops::Range<usize>)>,
    versions: Vec<([i32; 3], u64)>,
    bases: Vec<u32>,
    lights: lighting::Preparation,
    lighting_gpu: LightingGpu,
    instance_quad_bases: Buffer,
    columns: scene_upload::Columns,
}
struct PendingStorage {
    transfer: Option<Transfer>,
    anchor: [f64; 3],
    packed: Option<PackedRegions>,
    generation: u64,
    published: AccelerationStructure,
    stamps: Vec<([i32; 3], u64, u64)>,
    quads: Buffer,
    tokens: Buffer,
    vertices: Option<Buffer>,
    capacity: u32,
}
impl PendingStorage {
    fn matches(
        &self,
        source: prime_scene::RenderView<'_>,
        published: AccelerationStructure,
    ) -> bool {
        self.published == published
            && self.generation == source.generation
            && self.stamps.iter().copied().eq(source
                .regions
                .iter()
                .map(|r| (r.key, r.geometry_revision, r.content_revision)))
    }
    fn capture(
        source: prime_scene::RenderView<'_>,
        published: AccelerationStructure,
        quads: Buffer,
        tokens: Buffer,
        vertices: Option<Buffer>,
        capacity: u32,
        packed: PackedRegions,
        transfer: Transfer,
    ) -> Result<Self, Error> {
        let mut stamps = Vec::new();
        stamps
            .try_reserve_exact(source.regions.len())
            .map_err(scene_cache::Error::from)?;
        stamps.extend(
            source
                .regions
                .iter()
                .map(|r| (r.key, r.geometry_revision, r.content_revision)),
        );
        Ok(Self {
            generation: source.generation,
            transfer: Some(transfer),
            anchor: source.anchor,
            published,
            packed: Some(packed),
            stamps,
            quads,
            tokens,
            vertices,
            capacity,
        })
    }
}
struct RegionGpu {
    key: [i32; 3],
    revision: u64,
    baked_content: u64,
    fragments: Vec<FragmentSpan>,
    lamps: Option<Vec<lighting::Record>>,

    range: std::ops::Range<u32>,
    blas: Option<vulkan::TriangleBlas>,
    opaque: bool,
    opacity_slots: Vec<(u32, [u32; 2])>,
    resident: scene_cache::ResidentQuads,
}

/// 单次成功region发布的主机墙钟与工作集；三个主阶段互斥，细分字段仅作诊断。
/// 不包含宿主旧帧完成等待，也不是GPU timestamp；失败更新不发布快照。
#[derive(Clone, Copy, Default)]
pub struct SceneUpdateTiming {
    /// 变化区域从源 quad 到紧凑几何和属性的准备时间。
    pub geometry_ns: u64,
    /// 从后台领取的区域编码批墙钟，包括供页等待；不计入绘制线程。
    pub worker_geometry_ns: u64,
    /// 帧线程本轮交给 CPU 生产者的源字节，与 PCIe 上传分别计量。
    pub cpu_source_bytes: u64,
    /// 本次完成的批中需要重新识别与编码的 quad 数。
    pub encoded_quads: u64,
    /// 本帧送入展开与硬件 BLAS 构建的 quad 数。
    pub build_quads: u64,
    /// 本次完成的批中由不可变分片直接继承的 quad 数。
    pub reused_fragment_quads: u64,
    /// 是否发布了GPU场景或资源绑定；管理器空发布为false。
    pub changed: bool,
    /// 本批只传输部分地形，保留上一份场景直到跨帧输入完整。
    pub upload_pending: bool,
    /// 本次实际跨 CPU/GPU 传输的地形缓冲字节，包括实例、身份、OMM、灯表和补丁。
    pub terrain_upload_bytes: u64,
    /// BLAS 与 TLAS 准备的主机墙钟，同步入口另含完成等待。
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
    /// 压缩区域输入逻辑字节；分帧模式使用 terrain_upload_bytes 观察当帧实际传输。
    pub triangle_bytes: u64,
    /// 压缩区域输入的 CPU 保留容量。
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
    progress: history::Accumulation,
    jitter: [f32; 2],
    present: [ComputePipeline; 2],
    rr_enabled: bool,
    quality: streamline::Quality,
    rr: RrImages,
    pack: ComputePipeline,
    rr_present: [ComputePipeline; 2],
    rr_fallback: [ComputePipeline; 2],
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
            self.pack.into(),
        ];
        roots.extend(
            self.present
                .into_iter()
                .chain(self.rr_present)
                .chain(self.rr_fallback)
                .map(vulkan::native::Resource::from),
        );
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
        // 五个命中plane另加16B identity，供导引和照明共用首视线。
        let landing = [16, 16, 4, 8, 4, 16]
            .map(|stride| Buffer::new(context, pixels * stride, false))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let params = Buffer::new(context, 784, true)?;
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
        let present = display_variants(
            context,
            present_bindings,
            templates.map(|t| t.present),
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/present_present_manual.spv")),
                    c"present_manual",
                ),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/present_present_automatic.spv")),
                    c"present_automatic",
                ),
            ],
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
        let rr_present = display_variants(
            context,
            bindings,
            templates.map(|t| t.rr_present),
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_present_manual.spv")),
                    c"present_manual",
                ),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_present_automatic.spv")),
                    c"present_automatic",
                ),
            ],
        )?;
        let rr_fallback = display_variants(
            context,
            bindings,
            templates.map(|t| t.rr_fallback),
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_fallback_manual.spv")),
                    c"fallback_manual",
                ),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_fallback_automatic.spv")),
                    c"fallback_automatic",
                ),
            ],
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
            progress: history::Accumulation::new(),
            jitter: [0.; 2],
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

fn display_variants(
    context: &mut Context,
    bindings: Bindings,
    templates: Option<[ComputePipeline; 2]>,
    shaders: [RayShader<'static>; 2],
) -> Result<[ComputePipeline; 2], Error> {
    let mut create = |index: usize| match templates {
        Some(previous) => previous[index].rebind(context, bindings),
        None => ComputePipeline::new(context, bindings, shaders[index]),
    };
    Ok([create(0)?, create(1)?])
}

const fn shader(spirv: &'static [u8], entry: &'static std::ffi::CStr) -> RayShader<'static> {
    RayShader { spirv, entry }
}

const POST_PROGRAMS: [RayShader<'static>; 10] = [
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_plain_1.spv")),
        c"post_plain_1",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_plain_2.spv")),
        c"post_plain_2",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_clear_random_1.spv")),
        c"post_clear_random_1",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_clear_random_2.spv")),
        c"post_clear_random_2",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_clear_jittered_1.spv")),
        c"post_clear_jittered_1",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_clear_jittered_2.spv")),
        c"post_clear_jittered_2",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_shadowed_random_1.spv")),
        c"post_shadowed_random_1",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_shadowed_random_2.spv")),
        c"post_shadowed_random_2",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_shadowed_jittered_1.spv")),
        c"post_shadowed_jittered_1",
    ),
    shader(
        include_bytes!(concat!(env!("OUT_DIR"), "/post_shadowed_jittered_2.spv")),
        c"post_shadowed_jittered_2",
    ),
];

fn programs<'a>(
    callables: &'a [RayShader<'a>],
    invocation_reorder: bool,
    hit: materials::HitFeatures,
) -> Programs<'a> {
    Programs {
        realtime: None,
        diagnostic: None,
        prefix: Some(shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/prefix.spv")),
            c"prefix_main",
        )),
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
        material_hits: &[],
        callables,
        posts: &POST_PROGRAMS,
    }
}

fn program_identity<'a>(
    artifacts: impl IntoIterator<Item = &'a shader_build::Artifact>,
) -> blake3::Hash {
    let mut hash = blake3::Hasher::new();
    for artifact in artifacts {
        hash.update(&(artifact.bytes().len() as u64).to_le_bytes());
        hash.update(artifact.bytes());
    }
    hash.finalize()
}

// 每代 atlas、能力证明和已初始化程序一起发布；旧命令通过 Flight 保活旧绑定。
struct MaterialGeneration {
    dispatch: u32,
    programs: openpbr::program::ProgramSet<(openpbr::FlowKey, u32)>,

    hit: materials::HitFeatures,
    code: [blake3::Hash; 2],
    regular: Renderer,
    reordered: Option<Renderer>,
    reference_regular: Option<Renderer>,
    reference_reordered: Option<Renderer>,
    // Guide独立于PT照明管线；资源期准备两种SER设置，帧间仅重绑。
    guide_regular: Option<RayTracingPipeline>,
    guide_reordered: Option<RayTracingPipeline>,
}

impl MaterialGeneration {
    fn pt_program(&self, reordered: bool, reference: bool) -> &Renderer {
        if reference
            && let Some(program) = if reordered {
                self.reference_reordered.as_ref()
            } else {
                self.reference_regular.as_ref()
            }
        {
            return program;
        }
        if reordered {
            self.reordered.as_ref().expect("资源期已准备SER程序")
        } else {
            &self.regular
        }
    }

    fn roots(&self) -> impl Iterator<Item = vulkan::native::Resource> {
        self.regular
            .roots()
            .chain(self.reordered.iter().flat_map(Renderer::roots))
            .chain(self.reference_regular.iter().flat_map(Renderer::roots))
            .chain(self.reference_reordered.iter().flat_map(Renderer::roots))
            .chain(self.guide_regular.map(vulkan::native::Resource::from))
            .chain(self.guide_reordered.map(vulkan::native::Resource::from))
    }
}

#[cfg(test)]
fn atlas_flow(pixels: &[u8]) -> Result<openpbr::FlowKey, Error> {
    use simple_engine::openpbr::resource::{
        ColorScan, Coverage, Filtering, MaterialBinding, Texels,
    };
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
            max_bounces: bounces,
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
    // 显式诊断开关；只拆分 RR 之前不依赖宿主 command 的自有工作。
    diagnostic_sync: bool,
    #[cfg(test)]
    experiment_dispatch: u32,
    history_audit: history::Audit,

    tuning: prime_scene::RenderTuning,
    absolute_sample: u32,
    // next序号与尚未编码的原始record数量；仅完成全部记录后可接受。
    pending_absolute: Option<(u32, usize)>,
    // feature 必须先于 SDK 借用的 image 和设备退役。
    rr_runtime: Option<streamline::Runtime>,
    rr_unavailable: Option<streamline::Error>,
    rr_init: streamline::Init,
    rr_previous: Option<CameraFrame>,
    rr_index: u32,
    rr_quality: streamline::Quality,
    context: Context,
    attribute_slot_limit: u32,
    scene: Scene,
    anchor: [f64; 3],
    region_cache: scene_cache::Cache<RegionGpu>,
    terrain_uploads: vulkan::TerrainUploads,
    pending_storage: Option<PendingStorage>,
    cpu_packets: CpuPackets,
    cpu_ready: Option<PackedRegions>,
    attribute_revisions: Vec<([i32; 3], u64)>,
    scene_update: Option<SceneUpdateTiming>,
    // 程序模板只保留初始空场景，不能钉住重载时完整 mesh／AS 的工作集。
    program_scene: Scene,
    atlas: Image,
    material_gpu: MaterialGpu,
    opacity: opacity::Prepared,
    material_coverage: Vec<bool>,
    light_sides: Vec<u32>,
    light_weights: Vec<f32>,
    has_emission: bool,
    light_areas: Vec<[u32; 4]>,
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
    terrain_light: atmosphere::terrain::TerrainLight,
    terrain_input: Buffer,
    shadow_resolver: ComputePipeline,
    shadow_scene: (AccelerationStructure, Buffer, Buffer, Image, Buffer),
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
    guide_pipeline: Option<RayTracingPipeline>,

    known_geometry: Option<Buffer>,
    known_geometry_capacity: u32,

    geometry_placeholder: Buffer,

    flights: Vec<Flight>,
    // 测试宿主持有的native输出；生产输出由宿主管理，不进入资源表。
    #[cfg(test)]
    test_roots: Vec<vulkan::native::Resource>,
    #[cfg(test)]
    test_pt_timestamps: Option<vulkan::native::TimestampQueries>,
    #[cfg(test)]
    test_guide_timestamps: Option<vulkan::native::TimestampQueries>,
    #[cfg(test)]
    test_center_film: bool,
    #[cfg(test)]
    test_split_transport: bool,
    #[cfg(test)]
    test_jitter_start: u32,

    settings: pt::Settings,
    invocation_reorder: bool,
    display_origin: std::time::Instant,
    display_timeline: simple_engine::display::exposure::Timeline,
    display_drt: simple_engine::display::PrimeDrtParameters,
    offline: bool,
    offline_metered: bool,
}

impl Gpu {
    /// 宿主完成旧命令后发布固定采样偏好；输出概率/M cap热切换保留路径历史。
    /// CV切换只显式失效颜色历史。诊断仅在实时生效，切换时重启RGB累积，离线继续普通PT。
    /// # Errors
    /// 底层完成或资源状态错误原样返回。
    pub fn set_render_tuning(&mut self, tuning: prime_scene::RenderTuning) -> Result<(), Error> {
        if self.tuning.diagnostic() != tuning.diagnostic() && !self.offline {
            self.complete_previous()?;
            self.renderer = None;
            self.transition_history(history::Event::Accumulation(
                history::AccumulationEvent::OutputModeChanged,
            ))?;
        }
        self.tuning = tuning;
        Ok(())
    }

    const fn diagnostic(&self) -> bool {
        self.tuning.diagnostic() && !self.offline
    }
    /// 统一设置模式及实时积分器偏好；离线强制普通 PT，不接入 RR。
    /// 本入口维护模式与有效积分器的对应，宿主另负责冻结场景和相机；调用前须完成旧命令。
    pub fn set_rendering(&mut self, mode: prime_scene::RenderMode) -> Result<(), Error> {
        let offline = mode == prime_scene::RenderMode::Offline;

        if self.offline == offline {
            return Ok(());
        }
        self.complete_previous()?;
        let inherited_exposure = offline
            && self
                .view
                .as_ref()
                .is_some_and(|v| v.progress.samples() != 0);
        self.transition_history(history::Event::Global(GlobalReset::RenderDomainChanged))?;

        self.offline = offline;
        self.offline_metered = inherited_exposure;
        self.collect_resources()
    }

    /// 当前独立累积域已接受的样本数；尚无视图时为零，RR 每帧重开累积域。
    pub const fn accumulated_samples(&self) -> u32 {
        match &self.view {
            Some(view) => view.progress.samples(),
            None => 0,
        }
    }

    /// 世界退出后释放场景及模式工作集，保留已准备的资源世代；宿主须先完成旧命令。
    pub fn clear_world(&mut self) -> Result<(), Error> {
        self.cancel_packets()?;
        self.complete_previous()?;
        self.invalidate_history(GlobalReset::WorldReplaced)?;
        self.scene = Scene {
            top: self.program_scene.top,
            quads: self.program_scene.quads,
            tokens: self.program_scene.tokens,
            instance_quad_bases: self.program_scene.instance_quad_bases,
        };
        self.region_cache = scene_cache::Cache::new()?;
        self.terrain_uploads.clear();
        self.pending_storage = None;
        self.attribute_revisions.clear();
        self.lighting = lighting::Lighting::new()?;
        self.lighting_gpu = self.program_lighting;
        self.lighting_dirty = true;

        self.known_geometry = None;
        self.known_geometry_capacity = 0;

        self.view = None;
        self.offline_metered = false;
        self.anchor = [0.; 3];
        self.collect_resources()
    }

    /// 明确请求重新开始静态RGB累积；保留RR、绝对随机序列及曝光。
    pub fn restart_accumulation(&mut self) -> Result<(), Error> {
        self.complete_previous()?;
        self.transition_history(history::Event::Accumulation(
            history::AccumulationEvent::ExplicitRequested,
        ))
    }

    /// 单个 quad 属性缓冲允许的槽数，按分配器使用的二次幂容量向下取整。
    /// 空洞、场景内其他缓冲和实际可用显存另计。
    pub const fn attribute_slot_limit(&self) -> u32 {
        self.attribute_slot_limit
    }

    /// 启用保持原提交顺序的 NV 异步进度诊断；不插入逐阶段等待。
    /// # Safety
    /// 宿主已在借出的逻辑设备启用 `VK_NV_device_diagnostic_checkpoints`。
    pub unsafe fn enable_diagnostic_checkpoints(&mut self) -> Result<(), Error> {
        use std::io::Write as _;
        // SAFETY: 能力由宿主逻辑设备创建边界确认。
        unsafe { self.context.enable_diagnostic_checkpoints()? };
        writeln!(
            std::io::stderr().lock(),
            "[prime_pt] gpu_checkpoints enabled: asynchronous, no extra submits/waits"
        )
        .map_err(Error::DiagnosticOutput)?;
        Ok(())
    }

    /// 输出驱动检查点与近期 CPU 录制信息；不恢复设备或确认在途工作完成。
    /// # Safety
    /// 宿主已收到同设备的 `VK_ERROR_DEVICE_LOST`，保活设备和 queue，并独占 queue。
    pub unsafe fn report_device_loss(&self) -> Result<(), Error> {
        use std::io::Write as _;
        // SAFETY: 只从宿主明确的设备丢失异常边界调用，普通超时不调用。
        let report = unsafe { self.context.checkpoints_after_device_loss()? };
        let mut output = std::io::stderr().lock();
        let Some(report) = report else {
            writeln!(
                output,
                "[prime_pt] gpu_fault: asynchronous checkpoints were not enabled"
            )
            .map_err(Error::DiagnosticOutput)?;
            return Ok(());
        };
        writeln!(
            output,
            "[prime_pt] gpu_fault: driver_stages={} retained_markers={}",
            report.progress.len(),
            report.recorded.len()
        )
        .map_err(Error::DiagnosticOutput)?;
        for progress in report.progress {
            writeln!(
                output,
                "[prime_pt] gpu_fault stage={:#x} marker={} checkpoint={:?}",
                progress.stage, progress.marker, progress.checkpoint
            )
            .map_err(Error::DiagnosticOutput)?;
        }
        for record in report.recorded.iter().rev().take(16).rev() {
            writeln!(output, "[prime_pt] gpu_fault recorded_only={record:?}")
                .map_err(Error::DiagnosticOutput)?;
        }
        Ok(())
    }

    /// # Safety
    /// 句柄属于同一活动 Vulkan 1.4 设备，已启用 RT pipeline、RayQuery、device address 与 subgroupSizeControl。
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
        // SAFETY: 沿用宿主契约，设备已启用固定32-lane调度所需特性。
        unsafe {
            Self::attach_options(
                instance,
                physical,
                device,
                queue,
                family,
                queue_index,
                false,
                false,
                true,
                None,
            )
        }
    }

    /// `runtime_root` 指向已提取并校验的发行资源。开发宿主传 `None`，从当前工作目录向上定位项目源码并使用本机 Slang。
    /// 独立诊断运行目录可通过 `PRIME_PT_SOURCE_ROOT` 显式指定源码树；不影响发行资源来源。
    /// 发行资源的两套 shader 源码在会话期间保持有效，派生缓存写入同根 `cache/`。
    /// # Safety
    /// 句柄属于同一活动设备，已启用 RT pipeline、RayQuery、device address 与 position fetch；family/index
    /// 对应借出的 queue。宿主独占 queue，并在每次调用和销毁前等待前次命令完成。
    /// invocation_reorder 为 true 时设备须另启用 EXT SER 扩展与特性。
    /// opacity_micromap 为 true 时须启用 VK_KHR_opacity_micromap 与 micromap 特性。
    /// 设备须启用 Vulkan 1.3 subgroupSizeControl，compute 支持请求32-lane subgroup。
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
        opacity_micromap: bool,
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
            let current = match std::env::var_os("PRIME_PT_SOURCE_ROOT") {
                Some(path) => PathBuf::from(path),
                None => std::env::current_dir().map_err(Error::DevelopmentDirectory)?,
            };
            let root = current
                .ancestors()
                .find(|path| path.join("crates/renderer/shaders/primary.slang").is_file())
                .ok_or(Error::Invalid("开发宿主须从 Prime PT 源码目录启动"))?;
            (
                root.join("target"),
                root.join("crates/renderer/shaders"),
                root.join("crates/renderer")
                    .join(env!("PRIME_ENGINE_SHADERS_RELATIVE")),
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
            subgroup_size_control: true,
            capabilities: Capabilities {
                ray_tracing: true,
                position_fetch: true,
                invocation_reorder,
                opacity_micromap,
                ..Default::default()
            },
        };
        // SAFETY: 外部生命周期由导出会话契约维持；本对象不销毁宿主句柄。
        let mut context = unsafe { Context::from_raw(raw)? };
        context.enable_pipeline_cache(&cache.join("pipeline-cache"));
        context.prepare_quad_expansion()?;
        let attribute_slots = context.max_storage_buffer_range() / 64;
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
        let (scene, _) = Self::build_scene(&mut context, &Mesh::default())?;
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
        let terrain_input = Buffer::new(
            &mut context,
            atmosphere::terrain::TerrainUpdate::INPUT_BYTES as u64,
            true,
        )?;
        let model = atmosphere.terrain_model(&mut context)?;
        let terrain_light =
            atmosphere::terrain::TerrainLight::new(&mut context, model, terrain_input)?;
        let shadow_scene = (
            scene.top,
            scene.quads,
            scene.instance_quad_bases,
            atlas,
            material_gpu.table,
        );
        let shadow_resolver = terrain_light.resolver(
            &mut context,
            None,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/shadow_columns.spv")),
                c"shadow_columns",
            ),
            scene.top,
            &[
                Binding::StorageBuffer(scene.quads),
                Binding::StorageBuffer(scene.instance_quad_bases),
                Binding::SampledImage(atlas),
                Binding::StorageBuffer(material_gpu.table),
            ],
        )?;
        let geometry_placeholder = Buffer::mapped_device_local(&mut context, 64)?;

        let gpu = Self {
            diagnostic_sync: std::env::var_os("PRIME_PT_DIAGNOSTIC_SYNC")
                .is_some_and(|value| value == "1"),
            #[cfg(test)]
            experiment_dispatch: if context.capabilities().invocation_reorder {
                8
            } else {
                0
            },
            history_audit: history::Audit::default(),

            tuning: prime_scene::RenderTuning::DEFAULT,
            absolute_sample: 0,
            pending_absolute: None,
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
            rr_quality: streamline::Quality::Performance,
            context,
            attribute_slot_limit,
            scene,
            anchor: [0.0; 3],
            region_cache: scene_cache::Cache::new()?,
            terrain_uploads: vulkan::TerrainUploads::default(),
            pending_storage: None,
            cpu_packets: CpuPackets::new()?,
            cpu_ready: None,
            attribute_revisions: Vec::new(),
            scene_update: None,
            program_scene,
            atlas,
            material_gpu,
            opacity: opacity::Prepared::default(),
            material_coverage: definitions.iter().map(materials::tests_coverage).collect(),
            light_areas: vec![[u32::MAX, 0, 0, 0]; definitions.len()],
            light_sides: vec![0; definitions.len()],
            light_weights: materials::emission_weights(&definitions)?,
            has_emission: false,
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
            terrain_light,
            terrain_input,
            shadow_resolver,
            shadow_scene,
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
            guide_pipeline: None,

            known_geometry: None,
            known_geometry_capacity: 0,

            geometry_placeholder,

            flights: Vec::new(),
            #[cfg(test)]
            test_roots: Vec::new(),
            #[cfg(test)]
            test_pt_timestamps: None,
            #[cfg(test)]
            test_guide_timestamps: None,
            #[cfg(test)]
            test_center_film: false,
            #[cfg(test)]
            test_split_transport: true,
            #[cfg(test)]
            test_jitter_start: 0,

            settings: pt::Settings::default().validate()?,
            invocation_reorder: invocation_reorder && ser_requested,
            display_origin: std::time::Instant::now(),
            display_timeline,
            display_drt: simple_engine::display::PrimeDrtSettings::DEFAULT.prepare(1.0)?,
            offline: false,
            offline_metered: false,
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
        let changed = self.display_timeline.compensation() != auto_exposure;
        self.display_timeline.set_compensation(auto_exposure)?;
        if changed {
            self.offline_metered = false;
        }
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

    fn build_scene(context: &mut Context, mesh: &Mesh) -> Result<(Scene, Option<Buffer>), Error> {
        if mesh.quads.len() > 4 * 1024 * 1024 || mesh.quads.iter().any(|q| !q.valid()) {
            return Err(Error::Invalid("quad 非有限或超过 4M 面预算"));
        }
        let n = mesh.quads.len().max(1) as u64;
        let quads = Buffer::mapped_device_local(context, n * 48)?;
        let tokens = Buffer::mapped_device_local(context, n * 8)?;
        let instance_quad_bases = Buffer::upload(context, &[0; 4])?;
        let (top, vertices) = if mesh.quads.is_empty() {
            (context.acceleration_instances(&[])?, None)
        } else {
            let mut encoder = scene_cache::QuadEncoder::with_capacity(mesh.quads.len())?;
            for q in &mesh.quads {
                encoder.push(q.positions(), 0, q.attributes())?;
            }
            let owner = scene_cache::Cache::<()>::new()?;
            let mut update = owner.begin(0)?;
            let resident =
                scene_cache::ResidentQuads::prepare(encoder.finish(), None, update.parts_mut().2)?;
            let vertices = Buffer::new(context, n * 64, false)?;
            let input = vulkan::QuadBlasInput {
                quads: &resident,
                opaque: false,
                opacity: None,
                output: vulkan::QuadOutput {
                    attributes: Some(quads),
                    identities: Some(tokens),
                    vertices: Some(vertices),
                    ..Default::default()
                },
            };
            let instance = TriangleInstance {
                geometry: vulkan::BatchBlas::New(0),
                transform: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
                custom_index: 0,
            };
            (
                context.quad_scene_batch(&[input], &[instance])?.top,
                Some(vertices),
            )
        };
        Ok((
            Scene {
                top,
                quads,
                tokens,
                instance_quad_bases,
            },
            vertices,
        ))
    }

    /// 完成旧提交后选择已准备的照明与导引程序；保留数据和 PT/RR 历史。
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
        self.guide_pipeline = None;

        Ok(())
    }

    /// 资源变化后重绑阶段并重启静态PT积分；RR重投影与曝光适应继续使用已接受历史。
    #[cfg(test)]
    fn invalidate(&mut self) -> Result<(), Error> {
        self.complete_previous()?;
        self.renderer = None;
        self.guide_pipeline = None;

        self.transition_history(history::Event::Accumulation(
            history::AccumulationEvent::SceneChanged,
        ))
    }

    /// 调用者确认整个历史域失效；普通section、相机、照明和积分设置变化不调用。
    /// 保留曝光适应状态；入口仍要求宿主先完成旧命令并独占队列。
    pub fn invalidate_history(&mut self, reason: GlobalReset) -> Result<(), Error> {
        self.invalidate_histories(history::Invalidation::all(reason))
    }

    /// 只失效请求指定的累积或RR；保留随机身份、jitter与曝光。
    /// 宿主须先完成此前GPU命令，已有待接受帧按原提交结果处理。
    pub fn invalidate_histories(&mut self, request: history::Invalidation) -> Result<(), Error> {
        self.complete_previous()?;
        self.transition_history(history::Event::Selected(request))
    }

    /// 测量夹具选择共同随机与jitter起点；保留SDK单调帧号，历史失效另行指定。
    #[cfg(test)]
    fn test_restart_sample_sequence(&mut self, first: u32) -> Result<(), Error> {
        self.complete_previous()?;
        self.absolute_sample = first;
        self.test_jitter_start = self.rr_index;
        Ok(())
    }

    /// 实际编码成功（测试直接提交完成亦可）后消费一项待接受记录。
    fn recording_encoded(&mut self) {
        if let Some((_, remaining)) = &mut self.pending_absolute {
            *remaining = remaining.saturating_sub(1);
        }
    }

    fn complete_previous(&mut self) -> Result<(), Error> {
        self.context.poll_terrain()?;
        if self
            .pending_absolute
            .is_some_and(|(_, remaining)| remaining != 0)
        {
            return Err(pt::Error::PendingBatch.into());
        }
        for flight in self.flights.drain(..) {
            // SAFETY: 所有公开调用及drop前宿主已经完成此前command。
            unsafe {
                self.context.complete(flight)?;
            }
        }
        if std::mem::take(&mut self.atmosphere_pending) {
            self.atmosphere.submitted();
        }
        self.terrain_light.submitted();
        self.display_timeline.submitted();

        if let Some((next, _)) = self.pending_absolute.take() {
            if self.diagnostic_sync {
                eprintln!("[prime_pt] gpu_checkpoint sample_next={next} state=host_complete");
            }
            self.absolute_sample = next;
        }
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
        let definitions = materials::defaults();
        let prepared = materials::bindings(
            self.resource_generation,
            &definitions,
            [width, height],
            pixels,
            &[],
            &[],
        )?;
        let atlas = Image::new(
            &mut self.context,
            [width, height, 1],
            ImageFormat::Rgba8Srgb,
        )?;
        atlas.upload(&mut self.context, pixels)?;
        let (resources, generation) = self.prepare_material_generation(
            atlas,
            self.material_gpu,
            &prepared.keys,
            &definitions,
            self.opacity.map.is_some(),
        )?;
        self.invalidate_history(GlobalReset::SceneDomainReplaced)?;
        self.atlas = atlas;
        self.material_gpu = resources;
        self.material_coverage = definitions.iter().map(materials::tests_coverage).collect();
        self.generation = Some(generation);
        self.renderer = None;
        self.guide_pipeline = None;

        Ok(())
    }

    /// 完整资源世代的唯一准备与发布入口；不得由section、实例或逐帧路径调用。
    ///
    /// 全部图像、静态alias、材质范围、程序和可用SER变体成功后一次发布。
    /// 旧命令须已完成；调用后在绘制前提交同一资源世代的region视图。
    pub fn replace_resources(&mut self, source: ResourceUpload<'_>) -> Result<(), Error> {
        self.cancel_packets()?;
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
            {
                return Err(Error::Invalid("完整资源图像或材质域无效"));
            }
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
            let prepared = materials::bindings(
                source.generation,
                source.materials,
                source.extent,
                source.albedo,
                source.mers,
                source.auxiliary,
            )?;
            let definitions = prepared.definitions.as_ref();
            let keys = &prepared.keys;
            validate_aliases(definitions, source.emission_alias_words)?;
            let mut light_sides = Vec::new();
            light_sides
                .try_reserve_exact(source.materials.len())
                .map_err(scene_cache::Error::from)?;
            light_sides.extend(definitions.iter().zip(keys).map(|(m, (flow, _))| {
                if flow.emits() {
                    materials::emission_sides(m)
                } else {
                    0
                }
            }));
            let opacity = opacity::Prepared::new(&mut self.context, &source, definitions)?;
            let has_emission = light_sides.iter().any(|&side| side != 0);
            let light_weights = materials::emission_weights(definitions)?;
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
            let (resources, generation) = self.prepare_material_generation(
                atlas,
                resources,
                keys,
                definitions,
                opacity.map.is_some(),
            )?;
            let candidates: Vec<_> = generation
                .roots()
                .chain(opacity.map.map(vulkan::native::Resource::from))
                .collect();
            self.collect_with(&candidates)?;
            // 此后只移动已经准备的所有者；不得以清理失败向宿主报告“保留旧世代”。
            self.renderer = None;
            self.guide_pipeline = None;
            self.transition_history(history::Event::Global(GlobalReset::SceneDomainReplaced))?;
            self.atlas = atlas;
            self.material_gpu = resources;
            self.material_coverage = material_coverage;
            self.opacity = opacity;
            self.light_sides = light_sides;
            self.light_weights = light_weights;
            self.has_emission = has_emission;
            self.region_cache = region_cache;
            self.terrain_uploads.clear();
            self.pending_storage = None;
            self.attribute_revisions.clear();
            self.scene.top = self.program_scene.top;
            self.scene.instance_quad_bases = self.program_scene.instance_quad_bases;
            self.lighting = lighting;
            self.lighting_gpu = self.program_lighting;
            self.light_areas = light_areas;
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
        keys: &[materials::Key],
        definitions: &[MaterialDefinition],
        opacity_enabled: bool,
    ) -> Result<(MaterialGpu, MaterialGeneration), Error> {
        if keys.len() != definitions.len() {
            return Err(Error::Invalid("材质Flow与输入定义数量不匹配"));
        }
        #[cfg(test)]
        let dispatch = self.experiment_dispatch;
        #[cfg(not(test))]
        let dispatch = if self.context.capabilities().invocation_reorder {
            8
        } else {
            0
        };
        let prepare_started = std::time::Instant::now();
        let workers = match std::env::var("PRIME_PT_SHADER_JOBS") {
            Ok(value) => value
                .parse::<std::num::NonZeroUsize>()
                .map_err(|_| Error::Invalid("PRIME_PT_SHADER_JOBS须为正整数"))?,
            Err(std::env::VarError::NotPresent) => std::num::NonZeroUsize::new(
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .min(8),
            )
            .unwrap(),
            Err(_) => return Err(Error::Invalid("PRIME_PT_SHADER_JOBS须为正整数")),
        };
        // 空 OMM 世代无需 RayQuery 的 micromap 模式；有表时能力已由构建边界建立。
        let omm = if opacity_enabled {
            "-DPRIMEPT_OMM=1"
        } else {
            "-DPRIMEPT_OMM=0"
        };
        let mut batch = self.compiler.batch();
        let hit = materials::hit_features(definitions);
        let coverage = format!("-DPRIMEPT_COVERAGE_MASK={}", hit.coverage);
        let emitter_capabilities = materials::emitter_capabilities(definitions, keys);
        let emitters = format!("-DPRIMEPT_EMITTER_CAPABILITIES={emitter_capabilities}");
        let lights = if keys.iter().any(|(flow, _)| flow.emits()) {
            "-DPRIMEPT_LIGHT_MASK=3"
        } else {
            "-DPRIMEPT_LIGHT_MASK=1"
        };
        let hit_source = self.shader_source.with_file_name("pt_hit.slang");
        let program_source = if dispatch == 8 {
            &hit_source
        } else {
            &self.shader_source
        };
        let compile_programs = if dispatch == 8 {
            openpbr::program::ProgramSet::queue_hit_variants
        } else {
            openpbr::program::ProgramSet::queue_variants
        };
        let programs = compile_programs(
            &mut batch,
            keys.iter().copied(),
            openpbr::program::Template {
                source: program_source
                    .to_str()
                    .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                entry: if dispatch == 8 {
                    "shade_hit"
                } else {
                    "shade_opaque"
                },
                output: "flow.spv",
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &[
                    "-fp-mode",
                    "precise",
                    "-DOPENPBR_RECIPE_VARIANT=1",
                    lights,
                    &coverage,
                    &emitters,
                    omm,
                    "-capability",
                    "spvRayTracingPositionFetchKHR",
                ],
            },
        )?;
        let hit_args = [
            "-fp-mode",
            "precise",
            "-DOPENPBR_RECIPE_VARIANT=1",
            lights,
            &coverage,
            &emitters,
            omm,
        ];
        let primary_source = self.shader_source.with_file_name("primary_material.slang");
        let primary_materials = (dispatch == 8)
            .then(|| {
                openpbr::program::ProgramSet::queue_variants(
                    &mut batch,
                    keys.iter().copied(),
                    openpbr::program::Template {
                        source: primary_source
                            .to_str()
                            .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                        entry: "shade_primary",
                        output: "primary-material.spv",
                        target: shader_build::Target::Spirv("spirv_1_4"),
                        args: &hit_args,
                    },
                )
                .map_err(Error::from)
            })
            .transpose()?;
        let material_hits = if (2..=7).contains(&dispatch) {
            Some(openpbr::program::ProgramSet::queue_hit_variants(
                &mut batch,
                programs.plan().keys().iter().copied(),
                openpbr::program::Template {
                    source: self
                        .shader_source
                        .to_str()
                        .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                    entry: "shade_hit",
                    output: "flow-hit.spv",
                    target: shader_build::Target::Spirv("spirv_1_4"),
                    args: &hit_args,
                },
            )?)
        } else {
            None
        };
        let compile_stage = |batch: &mut shader_build::RuntimeBatch<'_>,
                             source: &Path,
                             entry,
                             stage,
                             output,
                             args: &[&str]| {
            let args: Vec<_> = args.iter().copied().chain([omm]).collect();
            batch.queue(&shader_build::Recipe {
                source: source.to_str().ok_or(shader_build::Error::Configuration(
                    "shader路径不是UTF-8".into(),
                ))?,
                entry,
                stage,
                output,
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &args,
            })
        };
        let dispatch_arg = format!("-DPRIMEPT_DISPATCH={dispatch}");
        // raygen不执行材质叶子；显式占位只用于同module中未选中的入口解析。
        let no_material = ["-DOPENPBR_FLOW=0", "-DOPENPBR_VARIANT=0xffffffffu"];
        let args = [
            "-fp-mode",
            "precise",
            "-capability",
            "spvShaderInvocationReorderEXT",
            "-capability",
            "spvRayTracingPositionFetchKHR",
            &dispatch_arg,
            &coverage,
            &emitters,
            no_material[0],
            no_material[1],
        ];
        let native_args: Vec<_> = args
            .iter()
            .copied()
            .chain((dispatch == 8).then_some("-DPRIMEPT_RECONSTRUCTION=0"))
            .chain(cfg!(test).then_some("-DPRIMEPT_REFERENCE_FILM=1"))
            .collect();
        let guided_args: Vec<_> = args
            .iter()
            .copied()
            .chain(["-DPRIMEPT_SEPARATE_GUIDES=1"])
            .collect();
        let ser_args: Vec<_> = native_args
            .iter()
            .copied()
            .chain(["-DPRIMEPT_SER=1"])
            .collect();
        let guided_ser_args: Vec<_> = guided_args
            .iter()
            .copied()
            .chain(["-DPRIMEPT_SER=1"])
            .collect();
        let fused = (dispatch != 0)
            .then(|| {
                compile_stage(
                    &mut batch,
                    program_source,
                    if dispatch == 8 {
                        "render_offline"
                    } else {
                        "render_fused"
                    },
                    "raygeneration",
                    "render-fused.spv",
                    &native_args,
                )
            })
            .transpose()?;
        let primary = if dispatch == 8 {
            ["primary_random", "primary_jittered"]
                .into_iter()
                .map(|entry| {
                    compile_stage(
                        &mut batch,
                        program_source,
                        entry,
                        "raygeneration",
                        "primary.spv",
                        &native_args,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let guide_rg = self.shader_source.with_file_name("guides.slang");
        let guided = (dispatch == 8)
            .then(|| {
                compile_stage(
                    &mut batch,
                    &guide_rg,
                    "guide_main",
                    "raygeneration",
                    "guides.spv",
                    &guided_args,
                )
            })
            .transpose()?;
        let guided_ser = (dispatch == 8 && self.context.capabilities().invocation_reorder)
            .then(|| {
                compile_stage(
                    &mut batch,
                    &guide_rg,
                    "guide_main",
                    "raygeneration",
                    "guides.spv",
                    &guided_ser_args,
                )
            })
            .transpose()?;
        let fused_ser = (dispatch != 0 && self.context.capabilities().invocation_reorder)
            .then(|| {
                compile_stage(
                    &mut batch,
                    program_source,
                    if dispatch == 8 {
                        "render_offline"
                    } else {
                        "render_fused"
                    },
                    "raygeneration",
                    "render-fused.spv",
                    &ser_args,
                )
            })
            .transpose()?;
        let guide_source = self.shader_source.with_file_name("guide_material.slang");
        let mut realtime_graphs = Vec::new();
        let mut reference_graphs = Vec::new();
        if dispatch == 8 {
            for (reordered, args) in [(false, &native_args), (true, &ser_args)] {
                if reordered && !self.context.capabilities().invocation_reorder {
                    continue;
                }
                for entry in ["render_rt1", "render_rt2_lane0", "render_rt2_lane1"]
                    .into_iter()
                    .chain(cfg!(test).then_some("render_rt2_merged"))
                {
                    realtime_graphs.push(compile_stage(
                        &mut batch,
                        program_source,
                        entry,
                        "raygeneration",
                        "render-realtime.spv",
                        args,
                    )?);
                }
                if cfg!(test) {
                    reference_graphs.push(compile_stage(
                        &mut batch,
                        program_source,
                        "render_diagnostic",
                        "raygeneration",
                        "render-reference.spv",
                        args,
                    )?);
                }
            }
        }
        let guide_hits = openpbr::program::ProgramSet::queue_hit_variants(
            &mut batch,
            programs
                .plan()
                .keys()
                .iter()
                .copied()
                .map(materials::scattering_key),
            openpbr::program::Template {
                source: guide_source
                    .to_str()
                    .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                entry: "guide_hit",
                output: "guide-hit.spv",
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &[
                    "-fp-mode",
                    "precise",
                    "-DOPENPBR_RECIPE_VARIANT=1",
                    &coverage,
                    "-capability",
                    "spvRayTracingPositionFetchKHR",
                ],
            },
        )?;
        let primary_guides = (dispatch == 8)
            .then(|| {
                openpbr::program::ProgramSet::queue_variants(
                    &mut batch,
                    programs
                        .plan()
                        .keys()
                        .iter()
                        .copied()
                        .map(materials::scattering_key),
                    openpbr::program::Template {
                        source: guide_source
                            .to_str()
                            .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                        entry: "guide_primary",
                        output: "primary-guide.spv",
                        target: shader_build::Target::Spirv("spirv_1_4"),
                        args: &hit_args,
                    },
                )
                .map_err(Error::from)
            })
            .transpose()?;
        eprintln!(
            "[prime_pt] shader_prepare begin tasks={} workers={}",
            batch.task_count(),
            workers
        );
        let mut compiled = batch.compile(workers)?;
        let compile_statistics = compiled.statistics();
        let programs = programs.finish(&mut compiled)?;
        let material_hits = material_hits
            .map(|set| set.finish(&mut compiled))
            .transpose()?;

        let guide_hits = guide_hits.finish(&mut compiled)?;
        let primary_materials = primary_materials
            .map(|set| set.finish(&mut compiled))
            .transpose()?;
        let primary_guides = primary_guides
            .map(|set| set.finish(&mut compiled))
            .transpose()?;
        let mut take = |ticket| compiled.take(ticket);
        let primary = primary
            .into_iter()
            .map(&mut take)
            .collect::<Result<Vec<_>, _>>()?;
        let fused = fused.map(&mut take).transpose()?;
        let fused_ser = fused_ser.map(&mut take).transpose()?;
        let realtime_graphs: Vec<_> = realtime_graphs
            .into_iter()
            .map(&mut take)
            .collect::<Result<_, _>>()?;
        let reference_graphs: Vec<_> = reference_graphs
            .into_iter()
            .map(&mut take)
            .collect::<Result<_, _>>()?;
        let guided = guided.map(&mut take).transpose()?;
        let guided_ser = guided_ser.map(&mut take).transpose()?;

        let mut hit_shaders: Vec<_> = if dispatch == 8 {
            programs.artifacts()
        } else {
            material_hits
                .as_ref()
                .map_or(&[][..], |set| set.artifacts())
        }
        .iter()
        .map(|a| RayShader {
            spirv: a.bytes(),
            entry: c"shade_hit",
        })
        .collect();
        if dispatch != 8 {
            hit_shaders.extend(guide_hits.artifacts().iter().map(|artifact| RayShader {
                spirv: artifact.bytes(),
                entry: c"guide_hit",
            }));
        }
        let table_words = definitions
            .len()
            .checked_mul(28)
            .ok_or(Error::Invalid("材质表尺寸溢出"))?;
        let mut table = Vec::<u32>::new();
        table
            .try_reserve_exact(table_words)
            .map_err(scene_cache::Error::from)?;
        for ((definition, &slot), (flow, _)) in
            definitions.iter().zip(programs.plan().bindings()).zip(keys)
        {
            let guide = guide_hits.plan().bindings()[slot as usize];
            if slot > u32::MAX >> 2 || guide > u32::MAX >> 2 {
                return Err(Error::Invalid("材质程序索引超过30bit"));
            }
            let mut words = definition.words();
            words[4][0] |= slot << 2;
            words[4][1] |= guide << 2;
            words[4][2] |= flow.bits() << 20;
            table.extend(words.into_iter().flatten());
        }
        resources.table = Buffer::upload(&mut self.context, bytemuck::cast_slice(&table))?;
        let descriptors = self.renderer_bindings(
            &self.program_view,
            &atlas,
            &self.program_scene,
            resources,
            self.program_lighting,
        );

        // 比较实际链接代码；局部Guide/RG修改不能借用未变化材质叶子的身份复用旧管线。
        let code = [
            program_identity(
                programs
                    .artifacts()
                    .iter()
                    .chain(material_hits.iter().flat_map(|set| set.artifacts()))
                    .chain(primary_materials.iter().flat_map(|set| set.artifacts()))
                    .chain(primary.iter())
                    .chain(fused.iter())
                    .chain(fused_ser.iter())
                    .chain(realtime_graphs.iter())
                    .chain(reference_graphs.iter())
                    .chain(guide_hits.artifacts().iter().filter(|_| dispatch != 8)),
            ),
            program_identity(
                guide_hits
                    .artifacts()
                    .iter()
                    .chain(primary_guides.iter().flat_map(|set| set.artifacts()))
                    .chain(guided.iter())
                    .chain(guided_ser.iter()),
            ),
        ];
        let same_code = self.generation.as_ref().is_some_and(|previous| {
            hit == previous.hit && dispatch == previous.dispatch && code[0] == previous.code[0]
        });
        let same_guide_code = self.generation.as_ref().is_some_and(|previous| {
            hit == previous.hit && dispatch == previous.dispatch && code[1] == previous.code[1]
        });
        let pipelines_started = std::time::Instant::now();
        let (trace, post) = self.allocate_bindings(descriptors)?;
        let callables: Vec<_> = programs
            .artifacts()
            .iter()
            .map(|artifact| RayShader {
                spirv: artifact.bytes(),
                entry: c"shade_opaque",
            })
            .collect();
        let primary_callables: Vec<_> = primary_materials
            .iter()
            .flat_map(|set| set.artifacts())
            .map(|a| RayShader {
                spirv: a.bytes(),
                entry: c"shade_primary",
            })
            .collect();
        let guide_callables: Vec<_> = primary_guides
            .iter()
            .flat_map(|set| set.artifacts())
            .map(|a| RayShader {
                spirv: a.bytes(),
                entry: c"guide_primary",
            })
            .collect();
        let primary_shaders: Vec<_> = primary
            .iter()
            .zip([c"primary_random", c"primary_jittered"])
            .map(|(a, entry)| RayShader {
                spirv: a.bytes(),
                entry,
            })
            .collect();
        let configured = |reordered, reference| {
            let mut result = self::programs(&callables, reordered, hit);
            if dispatch != 0 {
                result.prefix = None;
                let code = if reference {
                    &reference_graphs[usize::from(reordered)]
                } else if reordered {
                    fused_ser.as_ref().expect("SER完整图")
                } else {
                    fused.as_ref().expect("完整图")
                };
                result.transport = RayShader {
                    spirv: code.bytes(),
                    entry: if reference {
                        c"render_diagnostic"
                    } else if dispatch == 8 {
                        c"render_offline"
                    } else {
                        c"render_fused"
                    },
                };
                result.material_hits = &hit_shaders;
                if dispatch >= 2 {
                    result.callables = &[];
                }
            }
            if dispatch == 8 && !reference {
                let base = usize::from(reordered) * if cfg!(test) { 4 } else { 3 };
                result.realtime = Some(RealtimePrograms {
                    primary: &primary_shaders,
                    transport: RayShader {
                        spirv: realtime_graphs[base].bytes(),
                        entry: c"render_rt1",
                    },
                    transport_pair: Some([
                        RayShader {
                            spirv: realtime_graphs[base + 1].bytes(),
                            entry: c"render_rt2_lane0",
                        },
                        RayShader {
                            spirv: realtime_graphs[base + 2].bytes(),
                            entry: c"render_rt2_lane1",
                        },
                    ]),
                    merged: cfg!(test).then(|| RayShader {
                        spirv: realtime_graphs[base + 3].bytes(),
                        entry: c"render_rt2_merged",
                    }),
                    callables: &primary_callables,
                });
            }
            result
        };
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
                configured(false, false),
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
                        configured(true, false),
                        trace,
                        post,
                        [1, 1],
                    )?
                },
            )
        } else {
            None
        };

        let reference_regular = if cfg!(test) && dispatch == 8 {
            Some(Renderer::new(
                &mut self.context,
                configured(false, true),
                trace,
                post,
                [1, 1],
            )?)
        } else {
            None
        };
        let reference_reordered =
            if cfg!(test) && dispatch == 8 && self.context.capabilities().invocation_reorder {
                Some(Renderer::new(
                    &mut self.context,
                    configured(true, true),
                    trace,
                    post,
                    [1, 1],
                )?)
            } else {
                None
            };

        let guide_trace = self::programs(
            &[],
            false,
            materials::HitFeatures {
                authored_normals: false,
                ..hit
            },
        );
        let guide_groups: Vec<_> = std::iter::once(guide_trace.closest_hit)
            .chain(guide_hits.artifacts().iter().map(|a| RayShader {
                spirv: a.bytes(),
                entry: c"guide_hit",
            }))
            .collect();
        let prepare_guide = |context: &mut Context,
                             code: Option<&shader_build::Artifact>,
                             old: Option<RayTracingPipeline>| {
            code.map(|code| {
                if same_guide_code && let Some(old) = old {
                    old.rebind(context, trace)
                } else {
                    RayTracingPipeline::with_hit_groups(
                        context,
                        trace,
                        RayShader {
                            spirv: code.bytes(),
                            entry: c"guide_main",
                        },
                        guide_trace.miss,
                        &guide_groups,
                        guide_trace.any_hit,
                        &guide_callables,
                    )
                }
            })
            .transpose()
        };
        let guide_regular = prepare_guide(
            &mut self.context,
            guided.as_ref(),
            self.generation.as_ref().and_then(|g| g.guide_regular),
        )?;
        let guide_reordered = prepare_guide(
            &mut self.context,
            guided_ser.as_ref(),
            self.generation.as_ref().and_then(|g| g.guide_reordered),
        )?;

        eprintln!(
            "[prime_pt] shader_prepare complete tasks={} cache_hits={} compiled={} workers={} shader_ms={} pipeline_ms={} total_ms={} reuse_pt={} reuse_guides={}",
            compile_statistics.tasks,
            compile_statistics.cache_hits,
            compile_statistics.tasks - compile_statistics.cache_hits,
            compile_statistics.workers,
            compile_statistics.elapsed.as_millis(),
            pipelines_started.elapsed().as_millis(),
            prepare_started.elapsed().as_millis(),
            same_code,
            same_guide_code
        );
        Ok((
            resources,
            MaterialGeneration {
                dispatch,
                programs,

                hit,
                code,
                regular,
                reordered,
                reference_regular,
                reference_reordered,
                guide_regular,
                guide_reordered,
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
            self.transition_history(history::Event::Accumulation(
                history::AccumulationEvent::MediumChanged,
            ))?;
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
        self.cancel_packets()?;
        let (scene, vertices) = Self::build_scene(&mut self.context, mesh)?;
        self.invalidate()?;
        self.known_geometry = vertices;
        self.known_geometry_capacity = mesh.quads.len() as u32;
        self.scene = scene;
        self.region_cache = scene_cache::Cache::new()?;
        self.terrain_uploads.clear();
        self.pending_storage = None;
        self.attribute_revisions.clear();
        Ok(())
    }

    /// 发布已排序、唯一且完整的region借用视图；输入只在调用内有效。
    ///
    /// 宿主须已确认旧命令完成。几何revision决定BLAS，content revision决定属性写入；
    /// 新几何在 GPU 生成属性，内容更新仅写变化范围。扩容时在设备端复制旧数据。
    /// 管理器空发布不触碰GPU；anchor只改变TLAS。准备或映射失败保留旧目录和属性。
    pub fn set_regions(&mut self, source: prime_scene::RenderView<'_>) -> Result<(), Error> {
        self.scene_update = None;
        self.complete_previous()?;
        self.cancel_packets()?;
        let result = self.set_regions_inner(source, false);
        if result.is_ok() && self.scene_update.is_some_and(|timing| !timing.changed) {
            return result;
        }
        if result.is_err() {
            self.collect_resources()?;
        }
        result
    }

    /// 当前可绘制 TLAS 的世界原点。候选移动或上传时相机仍相对这个已发布原点。
    pub const fn scene_anchor(&self) -> [f64; 3] {
        self.anchor
    }

    /// 最近一次成功场景调用的墙钟及工作集；失败时为None，空发布保存零工作量。
    pub const fn scene_timing(&self) -> Option<SceneUpdateTiming> {
        self.scene_update
    }

    /// 按原生队列顺序发布 64³ 地形。变化页经有界通道交予后台编码池，未变分片复用源版本。
    /// 返回 false 时宿主保留管理器发布令牌并继续绘制旧覆盖。生产错误可能在后续领取时返回。
    /// 同一候选分帧传输期间固定其原点，发布后下一次调用可单独移动 TLAS。
    /// GPU 展开、AS、灯树及表补丁在同队列执行。
    /// # Errors
    /// 与同步场景入口相同；失败撤销候选，已排队的独占临时资源按 fence 退役。
    pub fn stream_regions(
        &mut self,
        mut source: prime_scene::RenderView<'_>,
    ) -> Result<bool, Error> {
        self.scene_update = None;
        if source.anchor.iter().any(|v| !v.is_finite()) {
            return Err(Error::Invalid("场景原点须有限"));
        }
        self.complete_previous()?;
        self.cpu_packets.handoff_bytes = 0;
        self.cpu_packets.encoded = 0;
        self.cpu_packets.reused = 0;
        self.terrain_uploads.begin_frame();
        let polling = std::time::Instant::now();
        let worker_ns = self.poll_packets(source)?;
        let handoff = elapsed_ns(polling);
        if self.cpu_packets.pending() {
            self.scene_update = Some(SceneUpdateTiming {
                upload_pending: true,
                geometry_ns: handoff,
                cpu_source_bytes: self.cpu_packets.handoff_bytes as u64,
                worker_geometry_ns: worker_ns,
                instances: source.regions.len(),
                ..Default::default()
            });
            return Ok(false);
        }
        if let Some(storage) = &self.pending_storage
            && storage.matches(source, self.scene.top)
        {
            source.anchor = storage.anchor;
        }
        let result = self.set_regions_inner(source, true);
        if let Some(t) = &mut self.scene_update {
            t.geometry_ns += handoff;
            t.worker_geometry_ns = worker_ns;
            t.build_quads = self.terrain_uploads.built_quads() as u64;
            t.cpu_source_bytes = self.cpu_packets.handoff_bytes as u64;
            t.encoded_quads = self.cpu_packets.encoded;
            t.reused_fragment_quads = self.cpu_packets.reused;
        }
        if result.is_err() || self.terrain_pending() {
            self.collect_resources()?;
        }
        result.map(|()| self.scene_update.is_some_and(|v| !v.upload_pending))
    }

    /// CPU 页或 GPU 上传仍在途。即使当前没有新编辑，宿主也须继续推进并保留旧覆盖。
    pub fn terrain_pending(&self) -> bool {
        self.cpu_packets.pending()
            || self.pending_storage.is_some()
            || self.terrain_uploads.pending()
    }

    /// 设置一帧可传的地形字节，默认 64 MiB。零值或非 4B 对齐返回错误。
    pub fn set_terrain_upload_budget(&mut self, bytes: usize) -> Result<(), Error> {
        self.terrain_uploads.set_limit(bytes)?;
        Ok(())
    }
    fn set_regions_inner(
        &mut self,
        source: prime_scene::RenderView<'_>,
        queued: bool,
    ) -> Result<(), Error> {
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
        if same_geometry
            && same_content
            && source.anchor == self.anchor
            && !self.scene_resources_dirty()
        {
            if self.terrain_uploads.pending() || self.pending_storage.is_some() {
                self.terrain_uploads.clear();
                self.pending_storage = None;
                self.collect_resources()?;
            }
            self.scene_update = Some(metrics);
            return Ok(());
        }
        let mut matches = region_match::regions(
            source.regions,
            self.region_cache.entries(),
            &self.attribute_revisions,
        )?;
        // 同一几何版本可以更换材质。颜色补丁不影响覆盖，其余属性更新只有在旧 BLAS
        // 跳过覆盖时检查是否仍可复用；下面另外核对已绑定 OMM 的 UV／材质身份。
        if !queued {
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
            for (region, matched) in source.regions.iter().zip(&mut matches) {
                if matched.attributes_dirty
                    && !region.mesh.attribute_only()
                    && self.opacity.map.is_some()
                    && let Some(index) = matched.reuse
                {
                    let actual = region.mesh.quads().enumerate().filter_map(|(i, q)| {
                        let pair = self.opacity.indices(q.material_id().index(), q.uv());
                        (pair != [vulkan::OPACITY_UNKNOWN; 2]).then_some((i as u32, pair))
                    });
                    if !actual.eq(self.region_cache.entries()[index]
                        .opacity_slots
                        .iter()
                        .copied())
                    {
                        matched.reuse = None;
                    }
                }
            }
        }
        let started = std::time::Instant::now();
        let packed = self
            .pending_storage
            .as_mut()
            .filter(|p| queued && p.matches(source, self.scene.top))
            .and_then(|p| p.packed.take())
            .or_else(|| self.cpu_ready.take());
        let PackedRegions {
            mut patches,
            mut update,
            pending,
            opacity_indices,
        } = if let Some(packed) = packed {
            packed
        } else if queued
            && matches
                .iter()
                .any(|m| m.reuse.is_none() || m.attributes_dirty)
        {
            self.start_packets(source, &matches)?;
            self.scene_update = Some(SceneUpdateTiming {
                upload_pending: true,
                geometry_ns: elapsed_ns(started),
                instances: source.regions.len(),
                ..Default::default()
            });
            return Ok(());
        } else {
            let mut update = self.region_cache.begin(source.regions.len())?;
            let (slots, entries, ids) = update.parts_mut();
            let dirty_triangles = source.regions.iter().zip(&matches).try_fold(
                0usize,
                |count, (region, matched)| {
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
                },
            )?;
            let mut opacity_indices = Vec::new();
            if self.opacity.map.is_some() {
                opacity_indices
                    .try_reserve_exact(dirty_triangles)
                    .map_err(scene_cache::Error::from)?;
            }
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
                let start = opacity_indices.len();
                let mut encoder = scene_cache::QuadEncoder::with_capacity(count as usize)?;
                let mut opaque = true;
                let mut opacity_slots = Vec::new();
                for (local, (quad, attributes)) in region
                    .mesh
                    .quads()
                    .zip(region.mesh.attributes())
                    .enumerate()
                {
                    let coverage = self
                        .material_coverage
                        .get(quad.material_id().index() as usize)
                        .ok_or(Error::Invalid("构建AS前quad记录或材质身份无效"))?;
                    if !quad.valid() {
                        return Err(Error::Invalid("构建AS前quad记录或材质身份无效"));
                    }
                    opaque &= !coverage;
                    encoder.push(quad.positions(), 0, attributes)?;
                    if self.opacity.map.is_some() {
                        let pair = self.opacity.indices(quad.material_id().index(), quad.uv());
                        opacity_indices.extend(pair);
                        if pair != [vulkan::OPACITY_UNKNOWN; 2] {
                            opacity_slots
                                .try_reserve(1)
                                .map_err(scene_cache::Error::from)?;
                            opacity_slots.push((local as u32, pair));
                        }
                    }
                }
                pending.push((entries.len(), start..opacity_indices.len()));
                let old = self
                    .region_cache
                    .entries()
                    .binary_search_by_key(&region.key, |e| e.key)
                    .ok()
                    .map(|index| &self.region_cache.entries()[index].resident);
                let resident = scene_cache::ResidentQuads::prepare(encoder.finish(), old, ids)?;
                metrics.triangle_bytes += resident.positions().bytes() as u64;
                metrics.triangle_capacity_bytes += resident.positions().retained_bytes() as u64;

                entries.push(scene_cache::Prepared::Built(RegionGpu {
                    key: region.key,
                    revision: region.geometry_revision,
                    baked_content: region.content_revision,
                    fragments: Vec::new(),
                    lamps: None,
                    range,
                    blas: None,
                    opaque,
                    opacity_slots,
                    resident,
                }));
            }
            PackedRegions {
                patches: Default::default(),
                update,
                pending,
                opacity_indices,
            }
        };
        let (_, entries, _) = update.parts_mut();
        let same_acceleration = same_geometry && pending.is_empty();
        let rebuild_top = !same_acceleration || source.anchor != self.anchor;
        metrics.dirty_regions = pending.len();
        let end = entries
            .iter()
            .map(|e| e.get(self.region_cache.entries()).range.end)
            .max()
            .unwrap_or(0);
        let grow = u64::from(end) * 48 > self.scene.quads.len();
        let packet_ns = elapsed_ns(started);
        let (quads, tokens, vertex_buffer, vertex_capacity, mut allocation_ns) =
            if let Some(saved) = self
                .pending_storage
                .as_ref()
                .filter(|v| queued && v.matches(source, self.scene.top))
            {
                (saved.quads, saved.tokens, saved.vertices, saved.capacity, 0)
            } else {
                let uploading = std::time::Instant::now();
                let quads = if grow {
                    let capacity = end
                        .max(4096)
                        .checked_next_power_of_two()
                        .ok_or(Error::Invalid("属性容量溢出"))?;
                    let buffer =
                        Buffer::mapped_device_local(&mut self.context, u64::from(capacity) * 48)?;
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
                        if queued {
                            self.context.enqueue_copy(
                                self.scene.quads,
                                buffer,
                                metrics.attribute_copy_bytes,
                            )?;
                        } else {
                            self.scene.quads.copy_prefix_to(
                                &mut self.context,
                                &buffer,
                                metrics.attribute_copy_bytes,
                            )?;
                        }
                    }
                    buffer
                } else {
                    self.scene.quads
                };

                let tokens = if u64::from(end) * 8 > self.scene.tokens.len() {
                    let buffer =
                        Buffer::mapped_device_local(&mut self.context, quads.len() / 48 * 8)?;
                    let retained = entries
                        .iter()
                        .filter_map(|e| match e {
                            scene_cache::Prepared::Reused(i) => {
                                Some(self.region_cache.entries()[*i].range.end)
                            }
                            _ => None,
                        })
                        .max()
                        .unwrap_or(0);
                    if retained != 0 {
                        if queued {
                            self.context.enqueue_copy(
                                self.scene.tokens,
                                buffer,
                                u64::from(retained) * 8,
                            )?;
                        } else {
                            self.scene.tokens.copy_prefix_to(
                                &mut self.context,
                                &buffer,
                                u64::from(retained) * 8,
                            )?;
                        }
                    }
                    buffer
                } else {
                    self.scene.tokens
                };

                let (vertex_buffer, vertex_capacity) =
                    self.prepare_vertices(entries, end, queued)?;
                let allocation_ns = elapsed_ns(uploading);

                (quads, tokens, vertex_buffer, vertex_capacity, allocation_ns)
            };
        let descriptors = std::time::Instant::now();
        let inputs: Vec<_> = pending
            .iter()
            .map(|(index, range)| {
                let entry = entries[*index].get(self.region_cache.entries());
                vulkan::QuadBlasInput {
                    quads: &entry.resident,
                    output: vulkan::QuadOutput {
                        first: entry.range.start,
                        region: entry.key,
                        attributes: Some(quads),
                        identities: Some(tokens),
                        vertices: vertex_buffer,
                        cells: None,
                    },
                    opaque: entry.opaque,
                    opacity: self
                        .opacity
                        .map
                        .filter(|_| !range.is_empty())
                        .map(|map| (map, &opacity_indices[range.clone()])),
                }
            })
            .collect();
        metrics.geometry_ns = packet_ns + elapsed_ns(descriptors);
        metrics.pending_capacity_bytes =
            (pending.capacity() * size_of::<(usize, std::ops::Range<usize>)>()) as u64;
        metrics.input_capacity_bytes =
            (inputs.capacity() * size_of::<vulkan::QuadBlasInput<'_>>()) as u64;
        let saved_transfer = self
            .pending_storage
            .as_mut()
            .filter(|p| queued && p.matches(source, self.scene.top) && p.anchor == source.anchor)
            .and_then(|p| p.transfer.take());
        let transfer = if let Some(saved) = saved_transfer {
            saved
        } else {
            let packing = std::time::Instant::now();
            let mut baked_lamps = std::collections::BTreeMap::new();
            let mut attributes = Vec::new();
            let dirty_count = source
                .regions
                .iter()
                .zip(entries.iter())
                .zip(&matches)
                .enumerate()
                .filter(|(i, ((_, e), m))| {
                    !patches.contains_key(i)
                        && matches!(e, scene_cache::Prepared::Reused(_))
                        && m.attributes_dirty
                })
                .map(|(_, v)| v)
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
            for (row, ((region, entry), matched)) in source
                .regions
                .iter()
                .zip(entries.iter())
                .zip(&matches)
                .enumerate()
            {
                if let Some(mut p) = patches.remove(&row) {
                    if let Some(lamps) = p.lamps.take() {
                        baked_lamps.insert(region.key, lamps);
                    }
                    let base = attributes.len();
                    if attributes.is_empty() {
                        attributes = p.attributes;
                    } else {
                        attributes
                            .try_reserve(p.attributes.len())
                            .map_err(scene_cache::Error::from)?;
                        attributes.append(&mut p.attributes);
                    }
                    ranges
                        .try_reserve(p.ranges.len())
                        .map_err(scene_cache::Error::from)?;
                    ranges.extend(
                        p.ranges
                            .into_iter()
                            .map(|(o, r)| (o, base + r.start..base + r.end)),
                    );
                    versions.push((region.key, region.content_revision));
                    continue;
                }
                let built = matches!(entry, scene_cache::Prepared::Built(_));
                let entry = entry.get(self.region_cache.entries());
                let dirty = !built && matched.attributes_dirty;
                if dirty {
                    if !built && region.mesh.attribute_only() {
                        for patch in region.mesh.attribute_ranges() {
                            attribute_update::pack_patch(
                                &entry.range,
                                patch.first,
                                patch.attributes(),
                                &mut attributes,
                                &mut ranges,
                            )?;
                        }
                    } else {
                        // 新几何已在AS输入边界校验；复用几何的普通属性候选仍需检查。
                        if !built {
                            for quad in region.mesh.quads() {
                                if !quad.valid()
                                    || quad.material_id().index() as usize >= self.light_sides.len()
                                {
                                    return Err(Error::Invalid(
                                        "quad记录或材质身份不属于当前资源世代",
                                    ));
                                }
                            }
                        }
                        attribute_update::pack_patch(
                            &entry.range,
                            0,
                            region.mesh.attributes(),
                            &mut attributes,
                            &mut ranges,
                        )?;
                    }
                }
                versions.push((region.key, region.content_revision));
            }
            metrics.attribute_capacity_bytes =
                (attributes.capacity() * size_of::<[[u32; 4]; 3]>()) as u64;
            metrics.attribute_pack_ns += elapsed_ns(packing);
            metrics.geometry_ns += metrics.attribute_pack_ns;
            let light_started = std::time::Instant::now();
            let light_inputs = entries.iter().zip(source.regions).map(|(e, r)| {
                let entry = e.get(self.region_cache.entries());
                let lamps = if let Some(rows) = baked_lamps.get(&r.key) {
                    Some(rows.as_slice())
                } else if entry.baked_content == r.content_revision {
                    entry.lamps.as_deref()
                } else {
                    None
                };
                (entry.range.start, lamps)
            });
            let lights = self.lighting.prepare_backend(
                source,
                light_inputs,
                lighting::Emitters {
                    sides: &self.light_sides,
                    areas: &self.light_areas,
                    weights: &self.light_weights,
                },
                self.has_emission,
                self.resource_generation,
                true,
            )?;
            metrics.light_pack_ns = elapsed_ns(light_started);
            metrics.geometry_ns += metrics.light_pack_ns;
            let light_started = std::time::Instant::now();
            let lighting_gpu = if let Some(tables) = &lights.tables {
                if queued {
                    let alloc = |ctx: &mut Context, n: usize| {
                        Buffer::mapped_device_local(ctx, n.max(16) as u64)
                    };
                    LightingGpu {
                        tree: self.lighting_gpu.tree,
                        alias: alloc(&mut self.context, tables.selection.len() * 16)?,
                        meta: alloc(&mut self.context, tables.meta.len() * 16)?,
                        regions: alloc(&mut self.context, tables.regions.len() * 16)?,
                        inverse: alloc(&mut self.context, tables.inverse.len() * 16)?,
                    }
                } else {
                    LightingGpu::upload(&mut self.context, Some(tables))?
                }
            } else {
                self.lighting_gpu
            };
            if lights.tables.is_some() {
                metrics.light_upload_ns = elapsed_ns(light_started);
                metrics.light_upload_bytes = lighting_gpu.alias.len()
                    + lighting_gpu.meta.len()
                    + lighting_gpu.inverse.len()
                    + lighting_gpu.regions.len();
                metrics.resources_ns += metrics.light_upload_ns;
            }
            // 实例身份只引用同序目录；属性槽可独立扩展。anchor变化和纯属性更新复用目录。
            let mut bases = Vec::new();
            if !same_acceleration {
                bases
                    .try_reserve_exact(entries.len())
                    .map_err(scene_cache::Error::from)?;
                bases.extend(
                    entries
                        .iter()
                        .map(|e| e.get(self.region_cache.entries()).range.start),
                );
            }
            let base_write =
                (!bases.is_empty()).then(|| (0, bytemuck::cast_slice(bases.as_slice())));
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
            let mut columns = scene_upload::Columns::default();

            if queued {
                if let Some((offset, bytes)) = base_write {
                    columns.append(instance_quad_bases, offset, bytes)?;
                }
                if let Some(t) = &lights.tables {
                    columns.append(lighting_gpu.alias, 0, bytemuck::cast_slice(&t.selection))?;
                    columns.append(lighting_gpu.meta, 0, bytemuck::cast_slice(&t.meta))?;
                    columns.append(lighting_gpu.regions, 0, bytemuck::cast_slice(&t.regions))?;
                    columns.append(lighting_gpu.inverse, 0, bytemuck::cast_slice(&t.inverse))?;
                } else {
                    let p = &lights.patches;
                    columns.rows(lighting_gpu.alias, &p.alias)?;
                    columns.rows(lighting_gpu.meta, &p.meta)?;
                    columns.rows(lighting_gpu.regions, &p.regions)?;
                    columns.rows(lighting_gpu.inverse, &p.inverse)?;
                }
            }
            Transfer {
                upload: Default::default(),
                attributes,
                ranges,
                versions,
                bases,
                lights,
                lighting_gpu,
                instance_quad_bases,
                columns,
            }
        };
        let Transfer {
            mut upload,
            attributes,
            ranges,
            versions,
            bases,
            lights,
            mut lighting_gpu,
            instance_quad_bases,
            columns,
        } = transfer;
        let mut attribute_source = None;
        let mut side_source = None;
        let mut leaf_source = None;
        if queued {
            let mut parts = Vec::new();
            if !attributes.is_empty() {
                parts.push(bytemuck::cast_slice(attributes.as_slice()));
            }
            let side_index = parts.len();
            if !columns.bytes.is_empty() {
                parts.push(columns.bytes.as_slice());
            }
            let leaf_index = parts.len();
            if let Some(p) = &lights.device
                && !p.leaves.is_empty()
            {
                parts.push(bytemuck::cast_slice(&p.leaves));
            }
            let staged = upload.stage(&mut self.context, &mut self.terrain_uploads, &parts)?;
            let Some(staged) = staged else {
                metrics.attribute_upload_bytes = self.terrain_uploads.transferred() as u64;
                metrics.triangle_bytes = 0;
                metrics.upload_pending = true;
                metrics.terrain_upload_bytes = self.terrain_uploads.transferred() as u64;
                self.pending_storage = Some(PendingStorage::capture(
                    source,
                    self.scene.top,
                    quads,
                    tokens,
                    vertex_buffer,
                    vertex_capacity,
                    PackedRegions {
                        patches,
                        update,
                        pending,
                        opacity_indices,
                    },
                    Transfer {
                        upload,
                        attributes,
                        ranges,
                        versions,
                        bases,
                        lights,
                        lighting_gpu,
                        instance_quad_bases,
                        columns,
                    },
                )?);
                self.scene_update = Some(metrics);
                return Ok(());
            };
            if !columns.bytes.is_empty() {
                side_source = Some(staged[side_index]);
            }
            if !attributes.is_empty() {
                attribute_source = Some(staged[0]);
            }
            leaf_source = staged.get(leaf_index).copied();
        }
        let top = if rebuild_top {
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
            let instance_ns = elapsed_ns(packing);
            metrics.attribute_pack_ns += instance_ns;
            metrics.geometry_ns += instance_ns;
            let accelerating = std::time::Instant::now();
            let batch = if queued {
                let allowance = self.terrain_uploads.remaining();
                let submitted = self.context.stream_quad_scene(
                    &mut self.terrain_uploads,
                    allowance,
                    &inputs,
                    &instances,
                )?;
                metrics.triangle_bytes = self.terrain_uploads.transferred() as u64;
                let Some(batch) = submitted else {
                    metrics.upload_pending = true;
                    metrics.terrain_upload_bytes = self.terrain_uploads.transferred() as u64;
                    self.pending_storage = Some(PendingStorage::capture(
                        source,
                        self.scene.top,
                        quads,
                        tokens,
                        vertex_buffer,
                        vertex_capacity,
                        PackedRegions {
                            patches,
                            update,
                            pending,
                            opacity_indices,
                        },
                        Transfer {
                            upload,
                            attributes,
                            ranges,
                            versions,
                            bases,
                            lights,
                            lighting_gpu,
                            instance_quad_bases,
                            columns,
                        },
                    )?);
                    self.scene_update = Some(metrics);
                    return Ok(());
                };
                batch
            } else {
                self.context.quad_scene_batch(&inputs, &instances)?
            };
            metrics.acceleration_ns = elapsed_ns(accelerating);
            metrics.tlas_ns = batch.tlas_prepare_ns;
            metrics.as_submit_wait_ns = batch.blas.timing.submit_wait_ns;
            if !inputs.is_empty() {
                metrics.batch = batch.blas.timing;
            }
            drop(inputs);
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
        drop(pending);
        if let Some(proposal) = &lights.device {
            let vertices = vertex_buffer.unwrap_or(self.geometry_placeholder);
            lighting_gpu.tree = if queued {
                self.context.enqueue_light_tree_from(
                    vertices,
                    &proposal.leaves,
                    proposal.anchor,
                    false,
                    leaf_source,
                )?
            } else {
                self.context
                    .build_light_tree(vertices, &proposal.leaves, proposal.anchor, false)?
            }
            .tree;
            metrics.light_upload_bytes += size_of_val(proposal.leaves.as_slice()) as u64;
        }
        let geometry_buffer = vertex_buffer.unwrap_or(self.geometry_placeholder);
        let bindings_changed = vertex_buffer != self.known_geometry
            || top != self.scene.top
            || tokens != self.scene.tokens
            || quads != self.scene.quads
            || instance_quad_bases != self.scene.instance_quad_bases
            || lighting_gpu != self.lighting_gpu;
        if bindings_changed {
            let collecting = std::time::Instant::now();
            let mut candidates = lighting_gpu.roots().to_vec();
            candidates.extend([
                vulkan::native::Resource::from(top),
                quads.into(),
                tokens.into(),
                instance_quad_bases.into(),
                geometry_buffer.into(),
            ]);
            candidates.extend(side_source.map(vulkan::native::Resource::from));
            candidates.extend(attribute_source.map(vulkan::native::Resource::from));
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
        let light_patch_bytes =
            (alias_writes.len() + meta_writes.len() + inverse_writes.len() + region_writes.len())
                as u64
                * 16;

        let writes: Vec<_> = ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&attributes[range.clone()])))
            .collect();
        let base_write = (!bases.is_empty()).then(|| (0, bytemuck::cast_slice(bases.as_slice())));
        let writes_attributes = !writes.is_empty() || base_write.is_some();
        if writes_attributes || light_patch_bytes != 0 || side_source.is_some() {
            let writing = std::time::Instant::now();
            let patches: &[(&Buffer, &vulkan::native::BufferWriteRanges<'_>)] = &[
                (&quads, &writes),
                (&instance_quad_bases, base_write.as_slice()),
                (&lighting_gpu.alias, &alias_writes),
                (&lighting_gpu.meta, &meta_writes),
                (&lighting_gpu.inverse, &inverse_writes),
                (&lighting_gpu.regions, &region_writes),
            ];
            if queued {
                if let Some(buffer) = attribute_source {
                    let copies: Vec<_> = ranges
                        .iter()
                        .map(|(to, r)| {
                            (
                                quads,
                                (r.start * 48) as u64,
                                *to as u64,
                                (r.len() * 48) as u64,
                            )
                        })
                        .collect();
                    self.context.enqueue_buffer_ranges(buffer, &copies)?;
                }
                if let Some(buffer) = side_source {
                    self.context
                        .enqueue_buffer_ranges(buffer, &columns.copies)?;
                }
            } else {
                Buffer::write_scattered_batches(&mut self.context, patches)?;
            }
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
            tokens,
            instance_quad_bases,
        };
        if matches
            .iter()
            .zip(source.regions)
            .any(|(m, r)| m.attributes_dirty && !r.mesh.attribute_only())
        {
            self.terrain_light.request_refresh();
        }
        self.known_geometry = vertex_buffer;
        self.known_geometry_capacity = vertex_capacity;

        self.anchor = source.anchor;
        self.attribute_revisions = versions;
        if bindings_changed {
            self.renderer = None;
            self.guide_pipeline = None;
        }
        self.transition_history(history::Event::Accumulation(
            history::AccumulationEvent::SceneChanged,
        ))?;
        metrics.publish_ns = elapsed_ns(publishing);
        metrics.resources_ns += metrics.publish_ns;
        metrics.changed = true;
        metrics.terrain_upload_bytes = if queued {
            self.terrain_uploads.transferred() as u64
        } else {
            metrics.batch.input_bytes
                + metrics.attribute_upload_bytes
                + metrics.light_upload_bytes
                + if rebuild_top {
                    metrics.instances as u64 * 64
                } else {
                    0
                }
        };
        self.terrain_uploads.clear();
        self.pending_storage = None;
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
    fn refresh_shadow_scene(&mut self) -> Result<(), Error> {
        let key = (
            self.scene.top,
            self.scene.quads,
            self.scene.instance_quad_bases,
            self.atlas,
            self.material_gpu.table,
        );
        if key != self.shadow_scene {
            self.shadow_resolver = self.terrain_light.resolver(
                &mut self.context,
                Some(self.shadow_resolver),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/shadow_columns.spv")),
                    c"shadow_columns",
                ),
                key.0,
                &[
                    Binding::StorageBuffer(key.1),
                    Binding::StorageBuffer(key.2),
                    Binding::SampledImage(key.3),
                    Binding::StorageBuffer(key.4),
                ],
            )?;
            self.shadow_scene = key;
            self.terrain_light.request_refresh();
        }
        Ok(())
    }

    fn collect_resources(&mut self) -> Result<(), Error> {
        self.refresh_shadow_scene()?;
        self.collect_with(&[])
    }

    fn collect_with(&mut self, extra: &[vulkan::native::Resource]) -> Result<(), Error> {
        let mut roots = vec![
            self.scene.top.into(),
            self.scene.quads.into(),
            self.scene.tokens.into(),
            self.scene.instance_quad_bases.into(),
            self.program_scene.top.into(),
            self.program_scene.quads.into(),
            self.program_scene.tokens.into(),
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
        roots.extend(self.opacity.map.map(vulkan::native::Resource::from));
        roots.extend(self.lighting_gpu.roots());
        roots.extend(self.program_lighting.roots());
        roots.extend(self.atmosphere.roots());
        roots.extend(self.program_view.roots());
        if let Some(view) = &self.view {
            roots.extend(view.roots());
        }
        if let Some(generation) = &self.generation {
            roots.extend(generation.roots());
        }
        if let Some(renderer) = &self.renderer {
            roots.extend(renderer.roots());
        }
        roots.extend(self.guide_pipeline.map(vulkan::native::Resource::from));
        roots.extend(self.terrain_light.roots());
        roots.push(self.terrain_input.into());
        roots.push(self.shadow_resolver.into());
        roots.extend([self.geometry_placeholder].map(vulkan::native::Resource::from));
        roots.extend(self.known_geometry.map(vulkan::native::Resource::from));

        #[cfg(test)]
        roots.extend(self.test_roots.iter().copied());
        roots.extend(self.terrain_uploads.roots());
        if let Some(v) = &self.pending_storage {
            roots.extend([v.quads, v.tokens].map(vulkan::native::Resource::from));
            roots.extend(v.vertices.map(vulkan::native::Resource::from));
            if let Some(t) = &v.transfer {
                roots.extend(t.upload.roots());
                roots.extend(t.lighting_gpu.roots());
                roots.push(t.instance_quad_bases.into());
            }
        }
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
            buffer(&self.known_geometry.unwrap_or(self.geometry_placeholder)),
            buffer(&lighting.inverse),
            buffer(&lighting.regions),
            buffer(&materials.emission_alias),
            buffer(&scene.instance_quad_bases),
            buffer(&scene.tokens),
            buffer(&lighting.tree),
        ]);
        let (terrain_frame, terrain_radiance, first_hits) = self.terrain_light.outputs();
        let post = vec![
            buffer(&view.params),
            buffer(&view.output),
            buffer(&view.accumulation),
            buffer(&view.counts),
            buffer(&view.prefix),
            Binding::SampledImage(*self.atmosphere.bindings().aerial_light),
            Binding::SampledImage(*self.atmosphere.bindings().aerial_transmittance),
            Binding::Sampler(self.linear),
            Binding::StorageBuffer(terrain_frame),
            Binding::SampledImage(terrain_radiance),
            Binding::StorageBuffer(first_hits),
        ];
        (trace, post)
    }

    fn allocate_bindings(
        &mut self,
        (mut trace, post): (Vec<Binding>, Vec<Binding>),
    ) -> Result<(Bindings, Bindings), Error> {
        trace.push(Binding::StorageBuffer(
            self.known_geometry.unwrap_or(self.geometry_placeholder),
        ));
        Ok((
            Bindings::new(&mut self.context, &trace)?,
            Bindings::new(&mut self.context, &post)?,
        ))
    }

    fn create_guide_pipeline(&mut self) -> Result<RayTracingPipeline, Error> {
        let view = self.view.as_ref().expect("已创建view");
        let (mut trace, _) = self.renderer_bindings(
            view,
            &self.atlas,
            &self.scene,
            self.material_gpu,
            self.lighting_gpu,
        );
        trace.push(Binding::StorageBuffer(
            self.known_geometry.unwrap_or(self.geometry_placeholder),
        ));
        let bindings = Bindings::new(&mut self.context, &trace)?;
        let generation = self.generation.as_ref().expect("完整资源世代");
        let program = if self.invocation_reorder {
            generation.guide_reordered
        } else {
            generation.guide_regular
        };
        Ok(program
            .expect("资源期已准备Guide程序")
            .rebind(&mut self.context, bindings)?)
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
        let program = generation.pt_program(self.invocation_reorder, self.diagnostic());
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
        let next_absolute = self
            .absolute_sample
            .checked_add(self.settings.samples)
            .ok_or(pt::Error::SampleOverflow)?;
        let diagnostic = self.diagnostic();
        if extent.into_iter().any(|n| n == 0 || n > 65536) {
            return Err(Error::Invalid("视图尺寸须属于 1..=65536"));
        }
        let mut result = Vec::with_capacity(4);
        let mut rr_reconfigured = false;
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
                let sky = atmosphere::View {
                    altitude_km: (0.5 + (camera.origin[1] - environment.sea_level) * 0.001)
                        .clamp(0.0, 36000.0),
                    forward: camera.forward[..3].try_into().unwrap(),
                    right: camera.right[..3].try_into().unwrap(),
                    up: camera.up[..3].try_into().unwrap(),
                    sun_direction: environment.sun_direction,
                    max_distance_km: 64.0,
                    radiance_scale: [solar_scale; 3],
                };
                self.atmosphere.prepare(&mut self.context, sky)?;
                self.refresh_shadow_scene()?;
                let active =
                    self.known_geometry.is_some() && self.initial_medium == [1., 0., 0., 0.];
                if let Some(update) = self.terrain_light.prepare(
                    sky,
                    camera.origin[..3].try_into().unwrap(),
                    std::array::from_fn(|i| self.anchor[i] + f64::from(camera.origin[i])),
                    active,
                )? {
                    self.terrain_input
                        .write(&mut self.context, 0, update.bytes())?;
                    result.push(
                        self.terrain_light
                            .record(&mut self.context, self.shadow_resolver)?,
                    );
                }
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
                // configure 与替换前，入口契约已确认前次 evaluate 完成。
                let input = if rr_enabled {
                    if self.rr_runtime.is_none() {
                        let init = streamline::Init { ..self.rr_init };
                        self.rr_runtime = Some(unsafe { streamline::Runtime::new(init)? });
                    }
                    rr_reconfigured = true;
                    self.rr_runtime
                        .as_mut()
                        .unwrap()
                        .configure(extent, self.rr_quality)?
                } else {
                    extent
                };
                let next = View::new_scaled(
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
                )?;
                if !rr_enabled {
                    self.rr_runtime = None;
                }
                self.transition_history(history::Event::ViewStorage(storage_event))?;
                self.view = Some(next);
                self.renderer = None;
                self.guide_pipeline = None;
            }

            let realtime_pt =
                !self.offline && !diagnostic && self.generation.as_ref().unwrap().dispatch == 8;
            let composite = if !sky_enabled || self.initial_medium != [1., 0., 0., 0.] {
                Composite::None
            } else if self.terrain_light.shadowed() {
                Composite::Shadowed
            } else {
                Composite::Clear
            };
            let frame_film = rr_enabled;
            #[cfg(test)]
            let frame_film = frame_film || self.test_center_film;
            let split = self.settings.samples == 2;
            #[cfg(test)]
            let split = split && self.test_split_transport;
            let frame_plan = FramePlan::new(
                realtime_pt,
                self.settings.samples,
                split,
                frame_film,
                composite,
            )?;
            if self.renderer.is_none() {
                self.renderer = Some(self.create_renderer()?);
            }
            if rr_enabled
                && self.generation.as_ref().unwrap().dispatch == 8
                && self.guide_pipeline.is_none()
            {
                self.guide_pipeline = Some(self.create_guide_pipeline()?);
            }

            let previous_camera = self.rr_previous;

            let current = CameraFrame::new(camera, self.anchor)?;
            let vectors = [camera.origin, camera.forward, camera.right, camera.up];
            let key = std::array::from_fn(|i| {
                if i < 16 {
                    vectors[i / 4][i % 4].to_bits()
                } else {
                    [
                        self.settings.seed,
                        self.settings.max_bounces,
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
            self.prepare_accumulation_history(key, rr_enabled || diagnostic)?;
            let view = self.view.as_mut().unwrap();
            let extent = view.extent;
            let batch = view.progress.begin(self.settings.samples)?;
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
                self.settings.max_bounces,
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
            let jitter_index = self.rr_index;
            #[cfg(test)]
            let jitter_index = jitter_index - self.test_jitter_start;
            let jitter = crate::reconstruction::jitter(
                jitter_index,
                view.extent[0],
                view.output_extent[0],
                rr_enabled,
            )?;
            #[cfg(test)]
            let jitter = if self.test_center_film {
                [0.; 2]
            } else {
                jitter
            };
            let film_mode = if rr_enabled { 2.0 } else { 0.0 };
            #[cfg(test)]
            let film_mode = if self.test_center_film && !rr_enabled {
                1.0
            } else {
                film_mode
            };
            view.jitter = jitter;
            words.extend(
                [
                    jitter[0],
                    jitter[1],
                    if self.rr_previous.is_some() { 1.0 } else { 0.0 },
                    film_mode,
                ]
                .map(f32::to_bits),
            );
            words.extend([
                self.absolute_sample,
                block_emission_scale.to_bits(),
                self.absolute_sample,
                0,
            ]);
            words.extend(
                previous_camera
                    .as_ref()
                    .unwrap_or(&current)
                    .camera_words(self.anchor)?
                    .map(f32::to_bits),
            );
            words.extend([
                view.output_extent[0],
                view.output_extent[1],
                u32::from(diagnostic)
                    | (u32::from(
                        self.generation
                            .as_ref()
                            .is_some_and(|g| g.hit.authored_normals),
                    ) << 2)
                    | (u32::from(realtime_pt) << 5),
                u32::from(!self.offline && self.settings.samples == 2),
            ]);
            words.extend(self.anchor.map(|v| ((v / 64.) as i32) as u32));
            words.push(0);
            words.extend(self.initial_medium.map(f32::to_bits));
            words.extend([
                0,
                0,
                self.generation
                    .as_ref()
                    .unwrap()
                    .programs
                    .plan()
                    .keys()
                    .len() as u32,
                0,
            ]);

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
            if rr_enabled
                && !realtime_pt
                && let Some(pipeline) = self.guide_pipeline
            {
                let mut commands = Commands::new(&self.context)?;
                #[cfg(test)]
                if let Some(queries) = self.test_guide_timestamps {
                    commands.reset_timestamps(queries)?;
                    commands.barrier();
                    commands.write_timestamp(queries, 0)?;
                }
                commands.trace(pipeline, view.extent)?;
                commands.barrier();
                #[cfg(test)]
                if let Some(queries) = self.test_guide_timestamps {
                    commands.write_timestamp(queries, 1)?;
                }
                result.push(commands.finish(&mut self.context)?);
            }
            {
                let renderer = self.renderer.as_ref().unwrap();
                #[cfg(test)]
                let (queries, guide_queries) =
                    (self.test_pt_timestamps, self.test_guide_timestamps);
                #[cfg(not(test))]
                let (queries, guide_queries) = (None, None);
                let recorded = if let Some(schedule) = frame_plan.realtime {
                    renderer.record_realtime(
                        &mut self.context,
                        self.guide_pipeline.filter(|_| rr_enabled),
                        RealtimeFrame {
                            schedule,
                            primary: frame_plan.primary,
                            post: frame_plan.post,
                            queries,
                            guide_queries,
                        },
                    )?
                } else if let Some(queries) = queries {
                    renderer.record_with_timestamps(
                        &mut self.context,
                        batch.samples(),
                        queries,
                        frame_plan.post,
                    )?
                } else {
                    renderer.record(&mut self.context, batch.samples(), frame_plan.post)?
                };
                result.push(recorded);
            }
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
            let pipeline = view.present[usize::from(self.display_timeline.enabled())]
                .rebind(&mut self.context, bindings)?;
            let frame = if self.offline {
                simple_engine::display::exposure::Frame::new(
                    extent,
                    true,
                    0.,
                    false,
                    true,
                    self.display_timeline.compensation(),
                )?
            } else {
                self.display_timeline
                    .prepare(extent, true, self.display_origin.elapsed())?
            };
            let meter = self.display_timeline.enabled() && (!self.offline || !self.offline_metered);
            if let Some(exposure) = view
                .display
                .record(&mut self.context, frame, false, meter)?
            {
                result.push(exposure);
            }
            if self.offline {
                self.offline_metered = true;
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
            if rr_reconfigured && self.rr_runtime.take().is_some() {
                self.transition_history(history::Event::RrStorage(
                    history::StorageEvent::RuntimeRecreated,
                ))?;
            }
            self.cancel_recordings(result)?;
            return Err(error);
        }
        self.pending_absolute = Some((next_absolute, result.len()));
        Ok(result)
    }

    fn cancel_recordings(&mut self, recordings: Vec<Recorded>) -> Result<(), Error> {
        self.terrain_light.cancel();
        self.pending_absolute = None;

        if self.offline {
            self.offline_metered = false;
        }
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
                Ok(flight) => {
                    self.flights.push(flight);
                    self.recording_encoded();
                }
                Err((error, recorded)) => {
                    self.pending_absolute = None;

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

    // 只用于 RR 前的自有记录；RR 和呈现继续按顺序编码进宿主 command。
    fn submit_diagnostic_recordings(&mut self, recordings: Vec<Recorded>) -> Result<(), Error> {
        let mut pending = recordings.into_iter().enumerate();
        let sample = self.absolute_sample;
        while let Some((record, recorded)) = pending.next() {
            let result = recorded.submit_diagnostic_and_wait(
                &mut self.context,
                |op, kind, extent, complete| {
                    let state = if complete { "complete" } else { "begin" };
                    eprintln!(
                        "[prime_pt] gpu_checkpoint sample={sample} record={record} op={op} kind={kind} extent={extent:?} state={state}"
                    );
                },
            );
            if let Err(error) = result {
                eprintln!(
                    "[prime_pt] gpu_checkpoint sample={sample} record={record} failed: {error}"
                );
                self.cancel_recordings(pending.map(|(_, record)| record).collect())?;
                return Err(error.into());
            }
            self.recording_encoded();
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
        self.context
            .set_diagnostic_scope(u64::from(self.absolute_sample));
        // SAFETY: prime_draw 借出本设备的 GENERAL storage view，并保活至本次命令完成。
        let target =
            unsafe { ExternalBinding::new(&self.context, view, ExternalKind::StorageImage)? };
        let diagnostic = self.diagnostic();
        let rr = options.rr && !self.offline && !diagnostic;
        let quality_changed = self.rr_quality != options.quality;
        let output_changed = self
            .view
            .as_ref()
            .is_some_and(|view| view.output_extent != extent);
        self.settings.samples = options.samples;
        self.settings.max_bounces = options.bounces;
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
        let mut rr_touched = false;
        let result = (|| -> Result<(), Error> {
            let rr = self.view.as_ref().unwrap().rr_enabled;
            let next_index = if rr {
                self.rr_index
                    .checked_add(1)
                    .ok_or(pt::Error::SampleOverflow)?
            } else {
                self.rr_index
            };
            if self.diagnostic_sync {
                eprintln!(
                    "[prime_pt] gpu_frame sample={} rr_index={} renderer=pt rr={rr} quality={:?} ser={} output={extent:?} input={:?} regions={} anchor={:?} camera={:?} records={}",
                    self.absolute_sample,
                    self.rr_index,
                    self.rr_quality,
                    self.invocation_reorder,
                    self.view.as_ref().unwrap().extent,
                    self.region_cache.entries().len(),
                    self.anchor,
                    camera.origin,
                    recordings.len(),
                );
            }
            if self.diagnostic_sync && rr {
                self.submit_diagnostic_recordings(std::mem::take(&mut recordings))?;
            } else {
                // SAFETY: 宿主独占正在 render pass 外录制的 command；会话持有 Flight 至完成。
                unsafe {
                    self.encode_recordings(std::mem::take(&mut recordings), command)?;
                }
            }
            if rr {
                let current = CameraFrame::new(camera, self.anchor)?;
                let reset = self.rr_previous.is_none();
                let view = self.view.as_ref().unwrap();
                let frame = current.frame(
                    command,
                    self.rr_index,
                    reset,
                    view.jitter,
                    self.rr_previous.as_ref().unwrap_or(&current),
                    view.rr.resources(&self.context)?,
                )?;
                rr_touched = true;
                // SAFETY: 宿主独占本设备有效的录制中 command；无检查点模式时不录入命令。
                unsafe { self.context.diagnostic_checkpoint(command, "rr/begin")? };
                if self.diagnostic_sync {
                    eprintln!(
                        "[prime_pt] gpu_checkpoint sample={} state=rr_record_begin",
                        self.absolute_sample
                    );
                }
                let evaluated = match unsafe { self.rr_runtime.as_mut().unwrap().evaluate(&frame) }
                {
                    Ok(()) => true,
                    Err(error) if error.unsafe_recording() => return Err(error.source.into()),
                    Err(error) => {
                        eprintln!("RR 本帧回退：{error}");
                        false
                    }
                };
                // SAFETY: 与 RR 前相同的宿主录制边界。
                unsafe { self.context.diagnostic_checkpoint(command, "rr/end")? };
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
                    view.rr_present[usize::from(self.display_timeline.enabled())]
                        .rebind(&mut self.context, bindings)?
                } else {
                    view.rr_fallback[usize::from(self.display_timeline.enabled())]
                        .rebind(&mut self.context, bindings)?
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
                if self.diagnostic_sync {
                    eprintln!(
                        "[prime_pt] gpu_checkpoint sample={} state=rr_present_encoded evaluated={evaluated}",
                        self.absolute_sample
                    );
                }
                if evaluated {
                    self.rr_previous = Some(current);
                } else {
                    self.transition_history(history::Event::RrEvaluationFailed)?;
                }
                self.rr_index = next_index;
            }
            self.collect_resources()?;
            self.atmosphere_pending = true;
            Ok(())
        })();
        if result.is_err() {
            self.display_timeline.cancel();
            self.cancel_recordings(recordings)?;
            self.transition_history(history::Event::Accumulation(
                history::AccumulationEvent::RecordingCancelled,
            ))?;
            if rr_touched {
                self.transition_history(history::Event::RrEvaluationFailed)?;
            }
        }
        result
    }
}

impl Gpu {
    /// 宿主确认此前 GPU 命令完成后，显式完成记录并释放 RR；失败保留底层错误与 SDK 所有者。
    /// 在移除会话前调用；重复关闭不重复释放 SDK，场景资源仍由本对象持有至析构。
    /// # Errors
    /// 尚有未编码命令、完成记录失败或 SDK 释放失败时返回来源，宿主不得按成功关闭继续销毁设备。
    pub fn close(&mut self) -> Result<(), Error> {
        self.complete_previous()?;
        if let Some(runtime) = &mut self.rr_runtime {
            runtime.close()?;
        }
        self.rr_runtime = None;
        Ok(())
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            // SDK 释放失败不能靠 Drop 重试后继续拆除其图像和设备依赖。
            self.context.retain_failed_shutdown();
            std::mem::forget(self.rr_runtime.take());
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr().lock(),
                "[prime_pt] renderer 退役失败：{error}"
            );
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
    use simple_engine::openpbr;

    #[test]
    fn invisible_atlas_color_cannot_enable_a_diffuse_path() {
        let flow = super::atlas_flow(&[255, 255, 255, 127, 0, 0, 0, 255]).unwrap();
        assert_eq!(
            flow.bits(),
            openpbr::FlowKey::DIELECTRIC_REFLECTION | openpbr::FlowKey::CONTINUOUS_REFLECTION,
        );
    }
}
