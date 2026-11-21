//! 宿主无关的quad、材质身份及相机输入；不拥有游戏源、调度器或GPU资源。
mod rendering;
pub use rendering::{RenderMode, RenderTuning};
use simple_engine::camera;
/// 场景输入域校验失败；相机数学保留底层来源。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 非有限、退化或超出声明域的场景输入。
    #[error("{0}")]
    Input(&'static str),
    /// 引擎相机坐标或投影不合法。
    #[error(transparent)]
    Camera(#[from] camera::Error),
    /// 场景元数据或候选缓冲分配失败。
    #[error(transparent)]
    Allocation(#[from] std::collections::TryReserveError),
}
/// 当前 mod 的不透明 OpenPBR 预设；两者共享相同 Flow 子集，无额外材质表读取。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum Material {
    /// 源 Minecraft 模型的默认粗糙表面，roughness=0.9。
    #[default]
    Opaque = 0,
    /// 同一不透明闭包的较光滑表面，roughness=0.25。
    Smooth = 1,
}

/// 资源世代内的材质表索引；0、1分别保留给粗糙及光滑的兼容预设。
///
/// 数值本身不证明所属资源域或表长。渲染发布边界必须与同一世代的材质表一并校验，
/// 资源替换不得将仍在使用的索引解释为另一份材质。身份不增加每quad的48B属性宽度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct MaterialId(u32);

impl MaterialId {
    /// 包装资源所有者分配的索引；范围和资源域由发布边界校验。
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// 返回完整32位表索引；不包含隐式标志位。
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl From<Material> for MaterialId {
    fn from(value: Material) -> Self {
        Self(value as u32)
    }
}

/// 一份共享材质的112B输入，每quad只保存一个MaterialId。
///
/// 基础参数沿用引擎OpenPBR布局。MERS/aux贴图使用与albedo相同的atlas UV；
/// uv_bounds限定当前sprite的实际源tile，供资源准备与局部法线UV翻译使用。
/// 所有颜色为线性BT.709，extinction的RGB为每米消光系数；不是sRGB或透射率。
/// 此类型只描述输入，资源所有者负责纹理域和实际取样范围的联合校验。
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct MaterialDefinition {
    /// RGB乘子与线性roughness，分别属于`[0,1]`；源albedo/tint在取样时相乘。
    pub base_roughness: [f32; 4],
    /// opaque IOR`[1,4]`、metalness`[0,1]`、薄层SSS`[0,1]`、非负薄介质厚度。
    pub surface: [f32; 4],
    /// 体介质IOR`[1,4]`及非负RGB消光；solid进入使用此值，退出回到宿主声明的外部介质。
    pub extinction: [f32; 4],
    /// 线性BT.709发光RGB乘子与非负强度；实际发光还乘源纹理/乘色并遵守覆盖。
    pub emission: [f32; 4],
    /// OpenPBR flags、发光sideMask(1前/2后/3双)、textureFlags、normalFlags。
    /// normalFlags bit0为作者法线，高31bit是静态发光alias的word偏移（0为uniform）。
    pub control: [u32; 4],
    /// 共享atlas中实际源sprite的u0/v0/u1/v1，必须非退化。
    pub uv_bounds: [f32; 4],
    /// 二层覆盖sprite的u0/v0/u1/v1。`OVERLAY` 时与底层共享归一化UV，
    /// 只有覆盖层应用quad tint，透明texel保留底层。
    pub overlay_bounds: [f32; 4],
}

impl Default for MaterialDefinition {
    fn default() -> Self {
        Self {
            base_roughness: [1., 1., 1., 0.9],
            surface: [1.5, 0., 0., 0.],
            extinction: [1.5, 0., 0., 0.],
            emission: [0.; 4],
            control: [0, 1, Self::CUTOUT, 0],
            uv_bounds: [0., 0., 1., 1.],
            overlay_bounds: [0.; 4],
        }
    }
}

impl MaterialDefinition {
    /// 原始材质图提供MERS：metalness/emission/perceptual roughness/subsurface。
    pub const MERS: u32 = 1;
    /// aux图提供normalXY/normal-distribution/IOR；材质通道不作颜色转换。
    pub const AUXILIARY: u32 = 2;
    /// 发光来自MERS的emission通道，否则只使用源emission乘子和albedo。
    pub const TEXTURED_EMISSION: u32 = 4;
    /// 源layer确实使用alpha覆盖阈值；opaque不因atlas alpha拒绝命中。
    pub const CUTOUT: u32 = 1 << 3;
    /// 源layer为translucent；与薄片/体介质语义分别解释，不等于cutout。
    pub const TRANSLUCENT: u32 = 1 << 4;
    /// 资源期证明该源面有重合反向关联；只接受本quad的正面，保留两侧独立属性。
    pub const FRONT_ONLY: u32 = 1 << 5;
    /// 同cull组、相同材质及UV的反向源对已合并为一个quad，真实双侧接受。
    pub const TWO_SIDED: u32 = 1 << 6;
    /// 不透明的无发光底层与二值alpha覆盖层共用一个quad。
    pub const OVERLAY: u32 = 1 << 7;
    /// 二层覆盖sprite提供独立MERS。要求 `OVERLAY`。
    pub const OVERLAY_MERS: u32 = 1 << 8;
    /// 辅助图 A 保存 LabPBR 原始 F0／金属编号，MERS 已在准备阶段转码。
    pub const LAB_SPECULAR: u32 = 1 << 9;
    /// 所有可达模型面共面，可使用薄层散射。
    pub const PLANAR_SHEET: u32 = 1 << 10;
    /// 几何能确认是薄片的面，包括已配对的正反面。
    pub const fn allows_thin_scattering(&self) -> bool {
        self.control[2] & (Self::FRONT_ONLY | Self::TWO_SIDED | Self::PLANAR_SHEET) != 0
    }
    /// normalFlags的bit0表示实际源中存在作者切线法线。
    pub const AUTHORED_NORMAL: u32 = 1;

    /// 输出七个16B原位属性块，GPU顺序同字段声明并保留全部浮点位值。
    pub const fn words(self) -> [[u32; 4]; 7] {
        [
            float_words(self.base_roughness),
            float_words(self.surface),
            float_words(self.extinction),
            float_words(self.emission),
            self.control,
            float_words(self.uv_bounds),
            float_words(self.overlay_bounds),
        ]
    }

    /// 检查有限、非负和离散标记域；纹理有效范围及Flow支持仍由资源发布边界验证。
    pub fn valid(self) -> bool {
        let finite = self
            .base_roughness
            .into_iter()
            .chain(self.surface)
            .chain(self.extinction)
            .chain(self.emission)
            .chain(self.uv_bounds)
            .chain(self.overlay_bounds)
            .all(f32::is_finite);
        let flags = self.control[2];
        finite
            && self
                .base_roughness
                .into_iter()
                .all(|v| (0. ..=1.).contains(&v))
            && (1. ..=4.).contains(&self.surface[0])
            && (0. ..=1.).contains(&self.surface[1])
            && (0. ..=1.).contains(&self.surface[2])
            && self.surface[3] >= 0.
            && (1. ..=4.).contains(&self.extinction[0])
            && self.extinction[1..].iter().all(|v| *v >= 0.)
            && self.emission.into_iter().all(|v| v >= 0.)
            && self.control[0] & !3 == 0
            && (1..=3).contains(&self.control[1])
            && flags & !2047 == 0
            && (flags & Self::LAB_SPECULAR == 0
                || flags & (Self::AUXILIARY | Self::MERS) == (Self::AUXILIARY | Self::MERS))
            && self.uv_bounds[0] < self.uv_bounds[2]
            && self.uv_bounds[1] < self.uv_bounds[3]
            && if flags & Self::OVERLAY != 0 {
                self.control[0] == 0
                    && self.control[3] == 0
                    && flags
                        & (Self::AUXILIARY
                            | Self::TEXTURED_EMISSION
                            | Self::CUTOUT
                            | Self::TRANSLUCENT)
                        == 0
                    && self.emission[3] == 0.
                    && self
                        .overlay_bounds
                        .into_iter()
                        .all(|v| (0. ..=1.).contains(&v))
                    && self.overlay_bounds[0] < self.overlay_bounds[2]
                    && self.overlay_bounds[1] < self.overlay_bounds[3]
            } else {
                flags & Self::OVERLAY_MERS == 0
            }
    }
}

const fn float_words(values: [f32; 4]) -> [u32; 4] {
    [
        values[0].to_bits(),
        values[1].to_bits(),
        values[2].to_bits(),
        values[3].to_bits(),
    ]
}

/// CPU quad：UV32、共享 tint/material16、四角 P48，共96B；普通 PT 的 shader 属性常驻前三块48B。
///
/// 固定两片 012/023；允许任意朝向、非共面和非平行四边形。三角面以 `[A,B,C,C]`
/// 表示，并复制第四角全部属性；AS 始终保留两输入槽，使 primitive>>1 直接寻址。
/// tint 为 quad 共享的未照明 sRGB；不支持逐顶点颜色、smooth normal 或第二组 UV。
/// 位置和 UV 保留完整 FP32 语义。传输边界只对可以无损还原的坐标和矩形使用紧凑格式。
/// GPU 将每个面的四角连续写入共用顶点列，供 AS、已知首命中和发光面使用。
/// 48B 着色属性独立存放，由区域基址加 primitive>>1 直接寻址。
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct Quad {
    blocks: [[u32; 4]; 6],
}

const _: () = assert!(std::mem::size_of::<Quad>() == 96);

impl Quad {
    /// 从真实四角生成六块记录；tint alpha 不参与覆盖率，UV 对应整张 atlas。
    pub const fn new(
        positions: [[f32; 3]; 4],
        uv: [[f32; 2]; 4],
        tint: [f32; 3],
        material: Material,
    ) -> Self {
        Self::with_material(positions, uv, tint, MaterialId::new(material as u32))
    }

    /// 从真实四角及资源材质索引构造；tint为未照明sRGB乘色，不参与alpha覆盖。
    /// 坐标、UV、乘色和索引合法性由对应资源发布边界校验。
    pub const fn with_material(
        positions: [[f32; 3]; 4],
        uv: [[f32; 2]; 4],
        tint: [f32; 3],
        material: MaterialId,
    ) -> Self {
        let [a, b, c, d] = positions;
        let [u0, u1, u2, u3] = uv;
        Self {
            blocks: [
                [
                    u0[0].to_bits(),
                    u0[1].to_bits(),
                    u1[0].to_bits(),
                    u1[1].to_bits(),
                ],
                [
                    u2[0].to_bits(),
                    u2[1].to_bits(),
                    u3[0].to_bits(),
                    u3[1].to_bits(),
                ],
                [
                    tint[0].to_bits(),
                    tint[1].to_bits(),
                    tint[2].to_bits(),
                    material.index(),
                ],
                [
                    a[0].to_bits(),
                    a[1].to_bits(),
                    a[2].to_bits(),
                    b[0].to_bits(),
                ],
                [
                    b[1].to_bits(),
                    b[2].to_bits(),
                    c[0].to_bits(),
                    c[1].to_bits(),
                ],
                [
                    c[2].to_bits(),
                    d[0].to_bits(),
                    d[1].to_bits(),
                    d[2].to_bits(),
                ],
            ],
        }
    }

    /// 返回原四角；CPU 仅在 dirty 合批和 AS 构建时解包，不逐帧转置。
    pub const fn positions(self) -> [[f32; 3]; 4] {
        let [a, b, c] = [self.blocks[3], self.blocks[4], self.blocks[5]];
        [
            [
                f32::from_bits(a[0]),
                f32::from_bits(a[1]),
                f32::from_bits(a[2]),
            ],
            [
                f32::from_bits(a[3]),
                f32::from_bits(b[0]),
                f32::from_bits(b[1]),
            ],
            [
                f32::from_bits(b[2]),
                f32::from_bits(b[3]),
                f32::from_bits(c[0]),
            ],
            [
                f32::from_bits(c[1]),
                f32::from_bits(c[2]),
                f32::from_bits(c[3]),
            ],
        ]
    }

    /// 返回四角 atlas 坐标，保留 UV 接缝与退化角的原值。
    pub const fn uv(self) -> [[f32; 2]; 4] {
        let [a, b] = [self.blocks[0], self.blocks[1]];
        [
            [f32::from_bits(a[0]), f32::from_bits(a[1])],
            [f32::from_bits(a[2]), f32::from_bits(a[3])],
            [f32::from_bits(b[0]), f32::from_bits(b[1])],
            [f32::from_bits(b[2]), f32::from_bits(b[3])],
        ]
    }

    /// 返回 quad 共享原 sRGB 乘色。
    pub const fn tint(self) -> [f32; 3] {
        [
            f32::from_bits(self.blocks[2][0]),
            f32::from_bits(self.blocks[2][1]),
            f32::from_bits(self.blocks[2][2]),
        ]
    }

    /// 返回属性记录中完整的资源材质索引；其所属表不由quad单独拥有。
    pub const fn material_id(self) -> MaterialId {
        MaterialId::new(self.blocks[2][3])
    }

    /// 将局部位置平移，保留材质及全部非位置位值。
    pub fn translate(&mut self, offset: [f32; 3]) {
        let positions = self
            .positions()
            .map(|p| std::array::from_fn(|i| p[i] + offset[i]));
        let mut changed = Self::new(positions, self.uv(), self.tint(), Material::Opaque);
        changed.blocks[2][3] = self.blocks[2][3];
        *self = changed;
    }

    /// 替换共享的未照明sRGB乘色，保留位置、UV和材质身份；调用者校验有限值。
    pub fn set_tint(&mut self, tint: [f32; 3]) {
        self.blocks[2][..3].copy_from_slice(&tint.map(f32::to_bits));
    }

    /// 返回三个连续16B的只读GPU属性块。
    pub const fn attributes(self) -> [[u32; 4]; 3] {
        [self.blocks[0], self.blocks[1], self.blocks[2]]
    }

    /// 检查当前记录的有限坐标、UV和tint；材质索引须另与所属资源表校验。
    pub fn valid(self) -> bool {
        self.positions()
            .iter()
            .flatten()
            .chain(self.uv().iter().flatten())
            .chain(self.tint().iter())
            .all(|v| v.is_finite())
    }
}

/// 各 section 的连续源 quad，构建边界负责打包并在 GPU 上安排物理顺序。
#[derive(Default)]
pub struct Mesh {
    /// 源序用于局部补丁。每个面保留两片，包括退化半面；跨帧对应使用驻留身份。
    pub quads: Vec<Quad>,
}

mod mesh_parts;
mod mesh_view;
pub use mesh_parts::{FragmentMask, IntoFragments, MeshParts, PreparedParts, TintPatch};
pub use mesh_view::{AttributeRange, AttributeRanges, Attributes, Fragments, MeshView, Quads};

/// 一份借用的64m region渲染输入；不包含源编译器或区块管理器所有权。
///
/// mesh位置相对key*64m。geometry_revision仅随实际位置/primitive顺序变化，
/// content_revision随任一quad属性或几何变化；同一身份的revision不得回退或复活旧内容。
#[derive(Clone, Copy)]
pub struct RenderRegion<'a> {
    /// 世界对齐64m region坐标，唯一且按字典序排列。
    pub key: [i32; 3],
    /// 同一region内实际位置与primitive顺序的单调几何世代。
    pub geometry_revision: u64,
    /// 同一region内全部几何及材质属性的单调内容世代。
    pub content_revision: u64,
    /// 本次调用期间保持不变的分片quad借用；调用结束后可由源所有者回收。
    pub mesh: MeshView<'a>,
}

/// 场景所有者借给一次渲染发布的完整非空region视图；不延长mesh的有效期。
/// 输入边界校验键唯一有序、坐标有限、revision及材质资源域后再发布GPU资源。
#[derive(Clone, Copy)]
pub struct RenderView<'a> {
    /// 已准备资源字典世代；零仅用于直接构造的兼容测试输入。
    pub generation: u64,
    /// 由调用者拥有的借用元数据切片；region mesh不复制。
    pub regions: &'a [RenderRegion<'a>],
    /// 全部实例及相机本次使用的绝对f64坐标原点。
    pub anchor: [f64; 3],
}

/// 同一场景世代内的稀疏渲染变化；数据借用持续到一次GPU准备返回。
#[derive(Clone, Copy)]
pub enum RenderChange<'a> {
    /// 完整region候选；仅颜色变化可以通过MeshView属性覆盖表达。
    Upsert(RenderRegion<'a>),
    /// 真实空输出或退出需求域；未知源不能用此项撤下旧覆盖。
    Remove([i32; 3]),
}

/// 主光线参数；right/up 已包含垂直 FOV 与宽高比，无矩阵约定依赖。
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct Camera {
    /// 当前场景 anchor 下的相对坐标；w 不使用。
    pub origin: [f32; 4],
    /// 视线中心方向；w 不使用。
    pub forward: [f32; 4],
    /// 屏幕向右半视锥向量；w 不使用。
    pub right: [f32; 4],
    /// 屏幕向上半视锥向量；w 不使用。
    pub up: [f32; 4],
}

/// 宿主相机 ABI：绝对位置保留 f64，方向为含 FOV/aspect 的半视锥。
///
/// 共 72 字节；位置占前 24 字节，随后为 forward/right/up 三个 float4。
/// GPU 转换必须先在 f64 中减去场景 anchor，不能先将绝对位置转为 f32。
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct WorldCamera {
    /// 宿主绝对位置，每世界单位为 1 m。
    pub origin: [f64; 3],
    /// 视线中心；w 保留为零。
    pub forward: [f32; 4],
    /// 向右半视锥；w 保留为零。
    pub right: [f32; 4],
    /// 向上半视锥；w 保留为零。
    pub up: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<WorldCamera>() == 72);

impl WorldCamera {
    /// 先以f64减anchor，再转换为已校验的追踪相机。
    pub fn relative(self, anchor: [f64; 3]) -> Result<Camera, Error> {
        let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
        let frame = camera::Frame::from_scaled(
            self.origin,
            xyz(self.forward),
            xyz(self.right),
            xyz(self.up),
        )
        .map_err(Error::from)?;
        let words = frame.relative(anchor).map_err(Error::from)?.words();
        let camera = Camera {
            origin: [words[0], words[1], words[2], 0.0],
            forward: self.forward,
            right: self.right,
            up: self.up,
        };
        camera.validate()?;
        Ok(camera)
    }

    /// 返回包住当前绝对位置的64m对齐anchor；拒绝非有限或超域位置。
    pub fn anchor(self) -> Result<[f64; 3], Error> {
        if self
            .origin
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 2_000_000_000.0)
        {
            return Err(Error::Input("宿主相机绝对位置越界"));
        }
        Ok(self.origin.map(|v| (v / 64.0).floor() * 64.0))
    }
}

impl Camera {
    /// 检查有限向量与非退化视锥，失败不修改相机。
    pub fn validate(&self) -> Result<(), Error> {
        let vectors = [self.origin, self.forward, self.right, self.up];
        if !vectors.iter().flatten().all(|x| x.is_finite()) {
            return Err(Error::Input("camera 含非有限值"));
        }
        let [f, r, u] = [self.forward, self.right, self.up].map(|v| [v[0], v[1], v[2]]);
        let cross = [
            r[1] * u[2] - r[2] * u[1],
            r[2] * u[0] - r[0] * u[2],
            r[0] * u[1] - r[1] * u[0],
        ];
        let determinant = (0..3).map(|i| f[i] * cross[i]).sum::<f32>();
        if !determinant.is_finite() || determinant.abs() < 1.0e-12 {
            return Err(Error::Input("camera 视锥退化"));
        }
        Ok(())
    }
}

/// 单帧天空与显示输入；五个连续 f32，太阳方向使用世界 +Y 向上。
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct Environment {
    /// 指向太阳的单位方向；零向量禁用太阳、天空和 Aerial。
    pub sun_direction: [f32; 3],
    /// 呈现曝光补偿，范围 -16..=16 EV；不改变 PT 或大气历史。
    pub exposure_ev: f32,
    /// 世界海平面 Y；海平面映射到 Earth 模型 0.5 km 高度。
    pub sea_level: f32,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            sun_direction: [0.0, 1.0, 0.0],
            exposure_ev: 0.0,
            sea_level: 63.0,
        }
    }
}

impl Environment {
    /// 校验曝光、海平面和太阳方向，并归一化容差内的单位方向。
    pub fn validated(mut self) -> Result<Self, Error> {
        if !self.sun_direction.iter().all(|v| v.is_finite())
            || !self.exposure_ev.is_finite()
            || !(-16.0..=16.0).contains(&self.exposure_ev)
            || !self.sea_level.is_finite()
        {
            return Err(Error::Input("environment 需要有限参数和 -16..=16 EV"));
        }
        let length = self.sun_direction.iter().map(|v| v * v).sum::<f32>().sqrt();
        if self.enabled() {
            if (length - 1.0).abs() > 0.001 || !length.is_finite() {
                return Err(Error::Input("太阳方向需要单位向量或禁用用零向量"));
            }
            self.sun_direction = self.sun_direction.map(|v| v / length);
        }
        Ok(self)
    }

    /// 是否启用当前世界的太阳和大气。
    pub fn enabled(self) -> bool {
        self.sun_direction != [0.0; 3]
    }
}

impl Quad {
    /// 精确比较AS所消费的位置位值，不含UV、tint或材质。
    pub fn same_geometry(&self, other: &Self) -> bool {
        self.blocks[3..] == other.blocks[3..]
    }
    /// 精确比较GPU材质属性位值，不含位置。
    pub fn same_attributes(&self, other: &Self) -> bool {
        self.blocks[..3] == other.blocks[..3]
    }
}
