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
//! 实时双样本与离线采用独立入口及最小链接集；有限film、合成和显示程序在准备阶段建立，
//! FramePlan统一选择录制与参数，帧内只重绑和选择已有程序。

#[path = "gpu_frame.rs"]
mod gpu_frame;
#[path = "gpu_nrd.rs"]
mod gpu_nrd;
#[path = "gpu_resources.rs"]
mod gpu_resources;
#[path = "gpu_scene.rs"]
mod gpu_scene;
#[path = "gpu_view.rs"]
mod gpu_view;
use gpu_nrd::NrdView;

use crate::{
    LensMode,
    bloom::BloomFilters,
    scene::{Camera, Environment, Mesh},
};
use prime_scene::MaterialDefinition;
#[cfg(test)]
use simple_engine::fft_bloom;
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
    /// 官方 NRD Vulkan 会话创建或录制失败。
    #[error(transparent)]
    Nrd(#[from] vulkan::nrd::Error),
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
    tree: vulkan::native::LightWorld,
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
            tree: context.empty_light_world()?,
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
    fn roots(self) -> [vulkan::native::Resource; 4] {
        [
            self.tree.into(),
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

enum HdrSource {
    Raw,
    Reconstructed,
    ScaledRaw,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Denoiser {
    Off,
    Dlss,
    Nrd,
}

struct View {
    nrd: Option<NrdView>,
    extent: [u32; 2],
    output_extent: [u32; 2],
    params: Buffer,
    output: Buffer,
    accumulation: Buffer,
    counts: Buffer,
    frame_local: bool,
    landing: Vec<Buffer>,
    signal_lanes: Buffer,
    optical_history: [Buffer; 2],
    prefix: Buffer,
    progress: history::Accumulation,
    jitter: [f32; 2],
    presentation: Presentation,
    rr_enabled: bool,
    quality: streamline::Quality,
    rr: RrImages,
    pack: ComputePipeline,
}

struct Presentation {
    params: Buffer,
    present: [ComputePipeline; 2],
    resolve: [ComputePipeline; 3],
    linear_hdr: Image,
    bloom: Option<vulkan::native::ImagePasses>,
    display: exposure::Display,
}

/// SDK 借用这些 image，所有权和退役边界仍在 mod 视图内。
/// RR 的九张图像按 bridge 顺序共用 56 B/输入像素和 8 B/输出像素，显露掩码另占 1 B/输入像素。
/// noisy/output 为未曝光的线性 BT.709；depth 为正 view-Z，运动为输入像素
/// previous-current。方向 albedo、法线与粗糙度由稳定导引给出，反射与透射各自重投影。
/// distance 供 mod 诊断，SDK 接收显式的 specular motion。
struct RrImages {
    images: [Image; 9],
    nrd_motion: [Image; 2],
    nrd_reflected_normal: Image,
    rr_specular_motion: Image,
    disocclusion: Image,
    previous_jitter: [f32; 2],
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

#[derive(Clone, Copy)]
/// 已校验的逐帧积分与重建设置；不拥有SDK或资源。
pub struct Options {
    denoiser: Denoiser,
    allow_nrd: bool,
    quality: streamline::Quality,
    samples: u32,
    bounces: u32,
}

impl Options {
    /// 校验离线批大小和路径预算；SDK档位只记录选择，在实际视图创建时协商。
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
            denoiser: if rr { Denoiser::Dlss } else { Denoiser::Off },
            allow_nrd: false,
            quality,
            samples,
            bounces,
        })
    }

    /// 原生分辨率下使用 SIGMA 与 REBLUR SH，覆盖 DLSS 选择。
    pub const fn with_nrd(mut self) -> Self {
        self.denoiser = Denoiser::Nrd;
        self
    }
    /// 宿主已启用 NRD 能力时，在 DLSS 不可用的情况下用原生 NRD 过滤同一场景。
    pub const fn with_nrd_fallback(mut self) -> Self {
        self.allow_nrd = true;
        self
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
    requested_nrd: bool,
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
    bloom_filter: BloomFilters,
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
    pub(super) const fn frame_samples(&self, offline_batch: u32) -> u32 {
        if self.offline {
            offline_batch
        } else {
            pt::REALTIME_SAMPLES
        }
    }

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
    /// 宿主仅在逻辑设备已开启 [vulkan::nrd::Runtime::new] 所需特性时选择 NRD 或 NRD 回退。
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
        let (cache, prime_shaders, engine_shaders, display_shaders) =
            if let Some(root) = runtime_root {
                (
                    root.join("cache"),
                    root.join("shaders/prime"),
                    root.join("shaders/engine"),
                    None,
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
                    Some(
                        root.join("crates/renderer")
                            .join(env!("PRIME_DISPLAY_SHADERS_RELATIVE")),
                    ),
                )
            };
        let mut includes = vec![prime_shaders.as_path(), engine_shaders.as_path()];
        includes.extend(display_shaders.as_deref());
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
        let bloom_filter = BloomFilters::new(&mut context)?;
        let program_view = View::new_scaled(
            &mut context,
            None,
            [[1, 1]; 2],
            false,
            Denoiser::Off,
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
            requested_nrd: false,
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
            bloom_filter,
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

    /// 选择显示变换前的泛光滤波器，`None` 关闭泛光。保留 PT、RR 与曝光历史，下一帧生效。
    /// 形状变化与关闭都撤下旧视图绑定，已录制命令按原有完成边界退役。
    pub fn set_lens_bloom(&mut self, mode: Option<LensMode>) {
        if self.bloom_filter.select(mode)
            && let Some(view) = &mut self.view
        {
            view.presentation.bloom = None;
        }
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
        if let Some(view) = &mut self.view {
            view.nrd = None;
        }
        Ok(())
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            // SDK 释放失败不能靠 Drop 重试后继续拆除其图像和设备依赖。
            self.context.retain_failed_shutdown();
            std::mem::forget(self.rr_runtime.take());
            if let Some(view) = &mut self.view {
                std::mem::forget(view.nrd.take());
            }
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
