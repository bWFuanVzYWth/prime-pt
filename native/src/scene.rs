//! PRT1 源包：4³ section 原子替换；Rust 生成直接寻址 quad，AS 输入另行展开。

use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};

pub(crate) const MAX_PACKET: usize = 256 * 1024 * 1024;
const MAX_VERTICES: usize = 16 * 1024 * 1024;
const QUAD_BYTES: usize = 84;

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

/// CPU quad：UV32、共享 tint/material16、四角 P48，共96B；shader 仅常驻前三块48B。
///
/// 固定两片 012/023；允许任意朝向、非共面和非平行四边形。三角面以 `[A,B,C,C]`
/// 表示，并复制第四角全部属性；AS 始终保留两输入槽，使 primitive>>1 直接寻址。
/// tint 为 quad 共享的未照明 sRGB；不支持逐顶点颜色、smooth normal 或第二组 UV。
/// 这些能力需要明确扩展格式，不能从 quad 假设推导。位置和 UV 保留全部 FP32 位值；
/// 不重建第四点，不使用体素坐标或隐式轴向。material 是真实预设选择，不作为对齐填充。
/// GPU 强制 KHR position fetch，通过 AS 获取实际三角位置；不另建常驻 P48 副本。
/// 48B 属性记录以 primitive>>1 直接寻址；AS 构建临时输入不属于此常驻属性预算。
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
                    material as u32,
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

    #[cfg(test)]
    pub(crate) fn translate(&mut self, offset: [f32; 3]) {
        let positions = self
            .positions()
            .map(|p| std::array::from_fn(|i| p[i] + offset[i]));
        let mut changed = Self::new(positions, self.uv(), self.tint(), Material::Opaque);
        changed.blocks[2][3] = self.blocks[2][3];
        *self = changed;
    }

    #[cfg(test)]
    pub(crate) fn set_tint(&mut self, tint: [f32; 3]) {
        self.blocks[2][..3].copy_from_slice(&tint.map(f32::to_bits));
    }

    pub(crate) const fn attributes(self) -> [[u32; 4]; 3] {
        [self.blocks[0], self.blocks[1], self.blocks[2]]
    }

    pub(crate) fn valid(self) -> bool {
        self.positions()
            .iter()
            .flatten()
            .chain(self.uv().iter().flatten())
            .chain(self.tint().iter())
            .all(|v| v.is_finite())
            && self.blocks[2][3] <= Material::Smooth as u32
    }
}

/// 所有 section 的连续 quad；无 shader 索引表，AS 展开临时数据由构建边界持有。
#[derive(Default)]
pub struct Mesh {
    /// 源 quad 顺序保持到 GPU primitive 身份，不删除退化半面。
    pub quads: Vec<Quad>,
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
    pub(crate) fn relative(self, anchor: [f64; 3]) -> Result<Camera, crate::Error> {
        let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
        let frame = camera::Frame::from_scaled(
            self.origin,
            xyz(self.forward),
            xyz(self.right),
            xyz(self.up),
        )
        .map_err(crate::gpu::Error::from)?;
        let words = frame
            .relative(anchor)
            .map_err(crate::gpu::Error::from)?
            .words();
        let camera = Camera {
            origin: [words[0], words[1], words[2], 0.0],
            forward: self.forward,
            right: self.right,
            up: self.up,
        };
        camera.validate()?;
        Ok(camera)
    }

    pub(crate) fn anchor(self) -> Result<[f64; 3], crate::Error> {
        if self
            .origin
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 2_000_000_000.0)
        {
            return Err(crate::Error::Input("宿主相机绝对位置越界"));
        }
        Ok(self.origin.map(|v| (v / 64.0).floor() * 64.0))
    }
}

impl Camera {
    pub(crate) fn validate(&self) -> Result<(), crate::Error> {
        let vectors = [self.origin, self.forward, self.right, self.up];
        if !vectors.iter().flatten().all(|x| x.is_finite()) {
            return Err(crate::Error::Input("camera 含非有限值"));
        }
        let [f, r, u] = [self.forward, self.right, self.up].map(|v| [v[0], v[1], v[2]]);
        let cross = [
            r[1] * u[2] - r[2] * u[1],
            r[2] * u[0] - r[0] * u[2],
            r[0] * u[1] - r[1] * u[0],
        ];
        let determinant = (0..3).map(|i| f[i] * cross[i]).sum::<f32>();
        if !determinant.is_finite() || determinant.abs() < 1.0e-12 {
            return Err(crate::Error::Input("camera 视锥退化"));
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
    pub(crate) fn validated(mut self) -> Result<Self, crate::Error> {
        if !self.sun_direction.iter().all(|v| v.is_finite())
            || !self.exposure_ev.is_finite()
            || !(-16.0..=16.0).contains(&self.exposure_ev)
            || !self.sea_level.is_finite()
        {
            return Err(crate::Error::Input(
                "environment 需要有限参数和 -16..=16 EV",
            ));
        }
        let length = self.sun_direction.iter().map(|v| v * v).sum::<f32>().sqrt();
        if self.enabled() {
            if (length - 1.0).abs() > 0.001 || !length.is_finite() {
                return Err(crate::Error::Input("太阳方向需要单位向量或禁用用零向量"));
            }
            self.sun_direction = self.sun_direction.map(|v| v / length);
        }
        Ok(self)
    }

    pub(crate) fn enabled(self) -> bool {
        self.sun_direction != [0.0; 3]
    }
}

struct Block<'a> {
    local: [u32; 3],
    offset: [f32; 3],
    quads: &'a [u8],
}

struct Section<'a> {
    key: [i32; 3],
    blocks: Vec<Block<'a>>,
    quads: usize,
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], crate::Error> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(crate::Error::Input("源包长度溢出"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(crate::Error::Input("源包截断"))?;
        self.offset = end;
        Ok(value)
    }

    fn word(&mut self) -> Result<u32, crate::Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

fn decode(bytes: &[u8]) -> Result<Vec<Section<'_>>, crate::Error> {
    if bytes.len() > MAX_PACKET {
        return Err(crate::Error::Input("源包超过 256 MiB"));
    }
    let mut reader = Reader { bytes, offset: 0 };
    if reader.word()? != 0x3154_5250 {
        return Err(crate::Error::Input("需要 PRT1 源包"));
    }
    let count = reader.word()? as usize;
    if count == 0 || count > 64 {
        return Err(crate::Error::Input("每批需要 1..=64 个 section"));
    }
    let mut sections = Vec::with_capacity(count);
    let mut keys = HashSet::with_capacity(count);
    let mut region = None;
    let mut total_quads = 0usize;
    for _ in 0..count {
        let key = [
            reader.word()? as i32,
            reader.word()? as i32,
            reader.word()? as i32,
        ];
        if key.iter().any(|v| v.checked_mul(16).is_none()) || !keys.insert(key) {
            return Err(crate::Error::Input("section 坐标越界或重复"));
        }
        let cell = key.map(|v| v.div_euclid(4));
        if region.is_some_and(|v| v != cell) {
            return Err(crate::Error::Input("section 不属于同一 4³ region"));
        }
        region = Some(cell);
        let blocks = reader.word()? as usize;
        if blocks > 4096 {
            return Err(crate::Error::Input("section 超过 4096 个方块"));
        }
        let mut section = Section {
            key,
            blocks: Vec::with_capacity(blocks),
            quads: 0,
        };
        let mut occupied = [0u64; 64];
        for _ in 0..blocks {
            let local = reader.word()? as usize;
            if local >= 4096 || occupied[local / 64] & (1 << (local % 64)) != 0 {
                return Err(crate::Error::Input("方块局部坐标越界或重复"));
            }
            occupied[local / 64] |= 1 << (local % 64);
            let quads = reader.word()? as usize;
            let offset = [reader.word()?, reader.word()?, reader.word()?].map(f32::from_bits);
            if !offset.iter().all(|v| v.is_finite()) {
                return Err(crate::Error::Input("模型偏移含非有限值"));
            }
            let data = reader.take(
                quads
                    .checked_mul(QUAD_BYTES)
                    .ok_or(crate::Error::Input("quad 数溢出"))?,
            )?;
            for quad in data.as_chunks::<QUAD_BYTES>().0 {
                if quad[4..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|v| !f32::from_le_bytes(*v).is_finite())
                {
                    return Err(crate::Error::Input("quad 含非有限坐标或 UV"));
                }
            }
            section.quads += quads;
            section.blocks.push(Block {
                local: [
                    (local & 15) as u32,
                    ((local >> 4) & 15) as u32,
                    (local >> 8) as u32,
                ],
                offset,
                quads: data,
            });
        }
        total_quads += section.quads;
        if total_quads > MAX_VERTICES / 4 {
            return Err(crate::Error::Input("批次几何超过顶点预算"));
        }
        sections.push(section);
    }
    if reader.offset != bytes.len() {
        return Err(crate::Error::Input("源包含多余字节"));
    }
    Ok(sections)
}

fn compile(section: &Section<'_>) -> Mesh {
    let mut mesh = Mesh {
        quads: Vec::with_capacity(section.quads),
    };
    for block in &section.blocks {
        let offset = std::array::from_fn::<_, 3, _>(|i| block.local[i] as f32 + block.offset[i]);
        for quad in block.quads.as_chunks::<QUAD_BYTES>().0 {
            let tint = u32::from_le_bytes(quad[..4].try_into().unwrap());
            let tint = [16, 8, 0].map(|shift| ((tint >> shift) & 255) as f32 / 255.0);
            let corners = std::array::from_fn::<_, 4, _>(|corner| {
                std::array::from_fn::<_, 5, _>(|i| {
                    let at = 4 + corner * 20 + i * 4;
                    f32::from_le_bytes(quad[at..at + 4].try_into().unwrap())
                })
            });
            mesh.quads.push(Quad::new(
                corners.map(|v| std::array::from_fn(|i| v[i] + offset[i])),
                corners.map(|v| [v[3], v[4]]),
                tint,
                Material::Opaque,
            ));
        }
    }
    mesh
}

/// section 缓存拥有 CPU mesh；工作池只在批次层调度，内核没有嵌套调度。
pub(crate) struct Scene {
    sections: BTreeMap<[i32; 3], SectionMesh>,
    pool: rayon::ThreadPool,
    anchor: [f64; 3],
    next_revision: u64,
    pub(crate) dirty: bool,
}

struct SectionMesh {
    mesh: Mesh,
    // 仅有序quad数量/P48位置位值变化分配新世代；属性更新不改变BLAS身份。
    revision: u64,
}

impl Scene {
    pub(crate) fn new() -> Result<Self, crate::Error> {
        let threads = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(8);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("prt-mesh-{i}"))
            .build()?;
        Ok(Self {
            sections: BTreeMap::new(),
            pool,
            anchor: [0.0; 3],
            next_revision: 1,
            dirty: true,
        })
    }

    pub(crate) fn clear(&mut self) {
        self.sections.clear();
        self.dirty = true;
    }

    pub(crate) fn rebase(&mut self, anchor: [f64; 3]) {
        if self.anchor != anchor {
            self.anchor = anchor;
            self.dirty = true;
        }
    }

    pub(crate) fn apply(&mut self, bytes: &[u8]) -> Result<(), crate::Error> {
        let sources = decode(bytes)?;
        let incoming = sources.iter().map(|s| s.quads).sum::<usize>();
        let replaced = sources
            .iter()
            .filter_map(|s| self.sections.get(&s.key))
            .map(|m| m.mesh.quads.len())
            .sum::<usize>();
        let current = self
            .sections
            .values()
            .map(|m| m.mesh.quads.len())
            .sum::<usize>();
        if current - replaced + incoming > MAX_VERTICES / 4 {
            return Err(crate::Error::Input("场景几何超过 16M 顶点预算"));
        }
        let sections = &self.sections;
        let build = |source: &Section<'_>| {
            let mesh = compile(source);
            let revision = sections
                .get(&source.key)
                .filter(|old| {
                    old.mesh.quads.len() == mesh.quads.len()
                        && old
                            .mesh
                            .quads
                            .iter()
                            .zip(&mesh.quads)
                            .all(|(old, new)| old.blocks[3..] == new.blocks[3..])
                })
                .map_or(0, |old| old.revision);
            SectionMesh { mesh, revision }
        };
        let meshes: Vec<_> = if sources.len() < 4 || incoming < 1024 {
            sources.iter().map(build).collect()
        } else {
            self.pool
                .install(|| sources.par_iter().map(build).collect())
        };
        let fresh = meshes
            .iter()
            .filter(|entry| !entry.mesh.quads.is_empty() && entry.revision == 0)
            .count();
        let next_revision = self
            .next_revision
            .checked_add(fresh as u64)
            .ok_or(crate::Error::Input("section revision溢出"))?;
        for (source, mut entry) in sources.into_iter().zip(meshes) {
            if entry.mesh.quads.is_empty() {
                self.sections.remove(&source.key);
            } else {
                if entry.revision == 0 {
                    entry.revision = self.next_revision;
                    self.next_revision += 1;
                }
                self.sections.insert(source.key, entry);
            }
        }
        self.next_revision = next_revision;
        self.dirty = true;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn merge(&self) -> Mesh {
        let mut result = Mesh {
            quads: Vec::with_capacity(self.sections.values().map(|m| m.mesh.quads.len()).sum()),
        };
        for (key, mesh) in &self.sections {
            let offset =
                std::array::from_fn(|i| (f64::from(key[i]) * 16.0 - self.anchor[i]) as f32);
            result.quads.extend(mesh.mesh.quads.iter().map(|quad| {
                let mut quad = *quad;
                quad.translate(offset);
                quad
            }));
        }
        result
    }

    pub(crate) const fn anchor(&self) -> [f64; 3] {
        self.anchor
    }

    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = ([i32; 3], u64, &Mesh)> {
        self.sections
            .iter()
            .map(|(&key, entry)| (key, entry.revision, &entry.mesh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_camera_keeps_far_origin_fraction() {
        let source = WorldCamera {
            origin: [30_000_000.125, 34.5, -30_000_000.25],
            forward: [0., 0., -1., 0.],
            right: [1., 0., 0., 0.],
            up: [0., 1., 0., 0.],
        };
        let anchor = source.anchor().unwrap();
        let relative = source.relative(anchor).unwrap();
        assert_eq!(relative.origin, [0.125, 34.5, 63.75, 0.]);
    }

    #[test]
    fn scene_anchor_preserves_local_geometry_at_world_edge() {
        let mut near = Scene::new().unwrap();
        near.apply(&packet([0, 0, 0], true)).unwrap();
        let mut far = Scene::new().unwrap();
        far.apply(&packet([1_875_000, 0, 0], true)).unwrap();
        far.rebase([30_000_000., 0., 0.]);
        assert_eq!(
            far.merge().quads[0].positions(),
            near.merge().quads[0].positions()
        );
    }

    #[test]
    fn arbitrary_quad_preserves_corner_bits_and_shared_attributes() {
        let positions = [
            [-0.0, 0.125, 3.0],
            [2.0, -0.25, 3.5],
            [1.25, 1.5, 2.75],
            [-0.5, 0.75, 4.0],
        ];
        let uv = [[-0.0, 0.25], [0.875, 0.5], [1.0, 0.75], [0.125, 1.0]];
        let tint = [0.2, -0.0, 1.0];
        let quad = Quad::new(positions, uv, tint, Material::Smooth);
        assert_eq!(
            quad.positions().map(|p| p.map(f32::to_bits)),
            positions.map(|p| p.map(f32::to_bits))
        );
        assert_eq!(
            quad.uv().map(|p| p.map(f32::to_bits)),
            uv.map(|p| p.map(f32::to_bits))
        );
        assert_eq!(quad.tint().map(f32::to_bits), tint.map(f32::to_bits));
        assert!(quad.valid());
    }

    fn packet(key: [i32; 3], quad: bool) -> Vec<u8> {
        let mut data = Vec::new();
        for word in [
            0x3154_5250,
            1,
            key[0] as u32,
            key[1] as u32,
            key[2] as u32,
            u32::from(quad),
        ] {
            data.extend(word.to_le_bytes());
        }
        if quad {
            for word in [
                0x321u32,
                1,
                0.25f32.to_bits(),
                0.0f32.to_bits(),
                (-0.5f32).to_bits(),
                0xff80_ff00,
            ] {
                data.extend(word.to_le_bytes());
            }
            for vertex in [
                [0.0f32, 0.0, 0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0, 1.0, 1.0],
                [0.0, 1.0, 0.0, 0.0, 1.0],
            ] {
                for value in vertex {
                    data.extend(value.to_le_bytes());
                }
            }
        }
        data
    }

    #[test]
    fn source_geometry_preserves_world_uv_and_tint() {
        let mut scene = Scene::new().unwrap();
        scene.apply(&packet([-1, 2, 3], true)).unwrap();
        let mesh = scene.merge();
        assert_eq!(mesh.quads[0].positions()[2], [-13.75, 35.0, 50.5]);
        assert_eq!(mesh.quads[0].uv()[2], [1.0, 1.0]);
        assert_eq!(mesh.quads[0].tint(), [128.0 / 255.0, 1.0, 0.0]);
        assert_eq!(mesh.quads.len(), 1);
    }

    #[test]
    fn empty_section_replaces_existing_geometry() {
        let mut scene = Scene::new().unwrap();
        scene.apply(&packet([0, 0, 0], true)).unwrap();
        scene.apply(&packet([0, 0, 0], false)).unwrap();
        assert!(scene.merge().quads.is_empty());
    }

    #[test]
    fn unchanged_source_preserves_geometry_revision() {
        let mut scene = Scene::new().unwrap();
        let bytes = packet([0, 0, 0], true);
        scene.apply(&bytes).unwrap();
        let revision = scene.entries().next().unwrap().1;
        scene.apply(&bytes).unwrap();
        assert_eq!(scene.entries().next().unwrap().1, revision);
    }

    #[test]
    fn malformed_batch_keeps_previous_scene() {
        let mut scene = Scene::new().unwrap();
        scene.apply(&packet([0, 0, 0], true)).unwrap();
        let mut invalid = packet([0, 0, 0], true);
        invalid.pop();
        assert!(scene.apply(&invalid).is_err());
        assert_eq!(scene.merge().quads.len(), 1);
    }

    #[test]
    fn negative_sections_use_euclidean_region() {
        let mut data = packet([-1, 0, 0], false);
        data[4..8].copy_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&packet([-4, 0, 0], false)[8..]);
        assert!(decode(&data).is_ok());
        let offset = 24;
        data[offset..offset + 4].copy_from_slice(&0i32.to_le_bytes());
        assert!(decode(&data).is_err());
    }

    #[test]
    fn environment_requires_unit_sun_or_explicit_disable() {
        let disabled = Environment {
            sun_direction: [0.; 3],
            ..Default::default()
        };
        assert!(!disabled.validated().unwrap().enabled());
        let invalid = Environment {
            sun_direction: [1., 1., 0.],
            ..Default::default()
        };
        assert!(invalid.validated().is_err());
    }

    #[test]
    #[ignore = "先运行 Gradle wireFixture，生成真实 Java BakedQuad 源包"]
    fn java_source_packet_preserves_geometry() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/java-source.prt");
        let bytes = std::fs::read(path).expect("请先运行 .\\gradlew.bat wireFixture");
        let mut scene = Scene::new().unwrap();
        scene.apply(&bytes).unwrap();
        let mesh = scene.merge();
        assert_eq!(mesh.quads.len(), 1);
        assert_eq!(mesh.quads[0].positions()[0], [-62.625, 2.5, 3.125]);
        assert_eq!(mesh.quads[0].uv()[0], [0.25, 0.5]);
        assert_eq!(
            mesh.quads[0].tint(),
            [128.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0]
        );
        assert_eq!(mesh.quads.len(), 1);
    }
}
