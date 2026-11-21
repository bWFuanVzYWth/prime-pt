//! PRM1资源字典；Java只转发实际模型拓扑、shape占用、sprite和状态声明。
//!
//! 首次加载和资源重载各准备一份完整字典。模型、材质、sprite 与 tint 配方的身份只在
//! 所属资源世代有效。编译任务借用稳定身份，资源准备及发布由 Catalog 所有者统一处理。

use crate::{Error, wire::Reader};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Placement {
    pub kind: u32,
    pub horizontal: f32,
    pub vertical: f32,
    pub seed: u32,
}

pub(crate) struct State {
    pub flags: u32,
    pub model: u32,
    pub name: String,
    pub faces: [u32; 6],
    pub fluid_kind: u32,
    pub fluid: u32,
    pub fluid_tint: Option<std::num::NonZeroU32>,
    pub level: u32,
    pub emission: u32,
    pub placement: Placement,
    pub face_mask: u32,
    pub matching_block: u32,
    pub blocks_motion: bool,
    pub leaves: Vec<ModelBinding>,
}

impl State {
    /// 宿主 MultifaceBlock 的薄片附着在相邻方块上，朝向本格内部的一侧外露。
    pub(crate) const MULTIFACE: u32 = 1 << 11;
}

pub(crate) struct BoundQuad {
    pub index: usize,
    pub material: prime_scene::MaterialId,
    pub tint: Option<std::num::NonZeroU32>,
}
pub(crate) struct ModelBinding {
    pub model: u32,
    pub quads: Vec<BoundQuad>,
}

pub(crate) struct SourceQuad {
    pub face: u32,
    pub tint: i32,
    pub layer: u32,
    pub sprite: u32,
    pub emission: u32,
    pub positions: [[f32; 3]; 4],
    pub uv: [[f32; 2]; 4],
}

pub(crate) enum Model {
    Unknown,
    Mesh(Vec<SourceQuad>),
    Weighted {
        children: Vec<(u32, u32)>,
        total: u32,
    },
    Multipart(Vec<u32>),
    Alias(u32),
}

pub(crate) struct Face {
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    pub words: Vec<u64>,
}

pub(crate) struct Fluid {
    pub layer: u32,
    pub tinted: bool,
    pub sprites: [u32; 3],
}

/// 源图像的实际RGBA8数据；无ICC/gamma变换，材质通道的解释由使用者明确选择。
pub struct ImageSource {
    /// 源图像宽度，非零。
    pub width: u32,
    /// 源图像高度，非零；动画图可为frame sheet。
    pub height: u32,
    /// 自持像素；静态albedo的空数组表示实际像素由共享atlas提供。
    pub pixels: Vec<u8>,
}

/// 资源世代内的源sprite；包含真实名称与获胜资源来源，不从RGB猜材质身份。
pub struct SpriteSource {
    /// 同一世代内稳定的非零sprite ID。
    pub id: u32,
    /// Java实际ResourceLocation，例如minecraft:block/stone。
    pub name: String,
    /// 对共享albedo atlas的u0/v0/u1/v1。
    pub bounds: [f32; 4],
    /// 单帧像素宽高。
    pub extent: [u32; 2],
    /// 获胜albedo来自vanilla包时为true，内嵌PBR预设只能在此条件使用。
    pub vanilla: bool,
    /// Minecraft是否在两帧之间插值。
    pub interpolate: bool,
    /// 按宿主顺序排列的(frame index,duration ticks)。
    pub frames: Vec<[u32; 2]>,
    /// 宿主mip/frame sheet；静态base可仅转发维度，避免重复atlas像素。
    pub images: Vec<ImageSource>,
    /// 资源包栈中获胜的 LabPBR 法线图及独立帧布局。
    pub normal: Option<crate::MaterialImage>,
    /// 宿主实际可选LabPBR specular源。
    pub specular: Option<crate::MaterialImage>,
}

/// 一次成功的完整资源世代发布；运行期不追加身份。
#[derive(Clone, Copy, Debug)]
pub struct ResourceChange {
    /// 当前成功发布的非零资源世代。
    pub generation: u64,
    /// true表示整个旧资源语义域被替换，旧mesh材质身份不再可解释。
    pub replaced: bool,
}

/// 独占Minecraft版本资源；无引用计数，编译只共享借用本所有者。
#[derive(Default)]
pub struct Catalog {
    generation: u64,
    pub(crate) states: HashMap<u32, State>,
    pub(crate) models: HashMap<u32, Model>,
    pub(crate) faces: HashMap<u32, Face>,
    pub(crate) fluids: HashMap<u32, Fluid>,
    pub(crate) sprites: HashMap<u32, SpriteSource>,
    pub(crate) fluid_materials: HashMap<(u32, usize), prime_scene::MaterialId>,
    pub(crate) definitions: Vec<prime_scene::MaterialDefinition>,
    tints: Option<crate::tint::TintResources>,
}

/// 已校验且预留提交容量的资源候选；独占借用阻止准备期间再次改变原Catalog。
/// 丢弃候选保留旧语义，GPU资源发布成功后才调用commit；像素不克隆旧字典。
pub struct PreparedCatalog<'a> {
    pub(crate) catalog: &'a mut Catalog,
    pub(crate) next: Catalog,
}

impl PreparedCatalog<'_> {
    /// 候选世代；不表示资源已发布。
    pub const fn generation(&self) -> u64 {
        self.next.generation
    }

    /// 借用候选世代的完整sprite源。
    pub fn sprites(&self) -> impl Iterator<Item = &SpriteSource> {
        self.next.sprites.values()
    }

    /// GPU资源发布已成功后移动候选；所有容量已准备，不再返回可恢复错误。
    pub fn commit(self) -> ResourceChange {
        *self.catalog = self.next;
        ResourceChange {
            generation: self.catalog.generation,
            replaced: true,
        }
    }
}

fn insert<T>(map: &mut HashMap<u32, T>, id: u32, value: T) -> Result<(), Error> {
    if map.insert(id, value).is_some() {
        return Err(Error::Input("同包资源ID重复"));
    }
    Ok(())
}

fn image(reader: &mut Reader<'_>, allow_empty: bool) -> Result<ImageSource, Error> {
    let width = reader.u32()?;
    let height = reader.u32()?;
    let count = reader.count(1)?;
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || count != width as usize * height as usize * 4 && !(allow_empty && count == 0)
    {
        return Err(Error::Input("源RGBA图像布局无效"));
    }
    Ok(ImageSource {
        width,
        height,
        pixels: reader.take(count)?.to_vec(),
    })
}

fn optional_image(
    reader: &mut Reader<'_>,
    base: &ImageSource,
    tile: [u32; 2],
    frames: &[[u32; 2]],
    interpolate: bool,
) -> Result<Option<crate::MaterialImage>, Error> {
    match reader.u32()? {
        0 => Ok(None),
        1 => {
            crate::MaterialImage::inherited(image(reader, false)?, base, tile, frames, interpolate)
                .map(Some)
        }
        2 => crate::MaterialImage::read(reader, base, tile, frames, interpolate).map(Some),
        _ => Err(Error::Input("可选源图像标记无效")),
    }
}

fn decode(bytes: &[u8]) -> Result<(Catalog, bool), Error> {
    let mut r = Reader::new(bytes)?;
    if r.u32()? != 0x314d_5250 || r.u32()? != 263 {
        return Err(Error::Input("需要Minecraft26.3 PRM1资源包"));
    }
    let generation = r.u64()?;
    let flags = r.u32()?;
    if generation == 0 || flags > 1 {
        return Err(Error::Input("资源世代或替换标记无效"));
    }
    let counts = [
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
        r.u32()? as usize,
    ];
    if counts
        .iter()
        .try_fold(0usize, |sum, n| sum.checked_add(*n))
        .is_none_or(|n| n > bytes.len() / 4)
    {
        return Err(Error::Input("资源记录数量超过包长度"));
    }
    let mut next = Catalog {
        generation,
        ..Catalog::default()
    };
    next.states.try_reserve(counts[0])?;
    next.models.try_reserve(counts[1])?;
    next.faces.try_reserve(counts[2])?;
    next.fluids.try_reserve(counts[3])?;
    next.sprites.try_reserve(counts[4])?;
    for _ in 0..counts[0] {
        let id = r.u32()?;
        let flags = r.u32()?;
        let model = r.u32()?;
        let name = r.string()?;
        let faces = [r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?];
        let support = r.u32()?;
        let fluid_kind = r.u32()?;
        let fluid = r.u32()?;
        let level = r.u32()?;
        let falling = r.u32()?;
        let emission = r.u32()?;
        let placement = Placement {
            kind: r.u32()?,
            horizontal: r.f32()?,
            vertical: r.f32()?,
            seed: r.u32()?,
        };
        if flags & !(2047 | State::MULTIFACE) != 0
            || fluid_kind > 3
            || level > 8
            || falling > 1
            || support & !(0x80000000 | 0x3ffff) != 0
            || emission > 15
            || placement.kind > 3
            || placement.seed > 6
            || placement.horizontal < 0.
            || placement.vertical < 0.
        {
            return Err(Error::Input("源方块状态声明无效"));
        }
        if placement.kind == 3 || placement.seed == 6 {
            return Err(Error::Input("自定义位置规则尚未接入批量宿主结果"));
        }
        insert(
            &mut next.states,
            id,
            State {
                flags,
                model,
                name,
                faces,
                fluid_kind,
                fluid,
                fluid_tint: None,
                level,
                emission,
                placement,
                face_mask: 127,
                matching_block: 0,
                blocks_motion: false,
                leaves: Vec::new(),
            },
        )?;
    }
    for _ in 0..counts[1] {
        let id = r.u32()?;
        if id == 0 {
            return Err(Error::Input("模型ID不能为零"));
        }
        let model = match r.u32()? {
            0 => Model::Unknown,
            1 => {
                let count = r.count(100)?;
                let mut quads = Vec::new();
                quads.try_reserve_exact(count)?;
                for _ in 0..count {
                    let face = r.u32()?;
                    let tint = r.i32()?;
                    let layer = r.u32()?;
                    let sprite = r.u32()?;
                    let emission = r.u32()?;
                    if face > 6 || tint < -1 || layer > 2 || sprite == 0 || emission > 15 {
                        return Err(Error::Input("源模型quad描述无效"));
                    }
                    let mut positions = [[0.; 3]; 4];
                    let mut uv = [[0.; 2]; 4];
                    for point in &mut positions {
                        for axis in point {
                            *axis = r.f32()?;
                        }
                    }
                    for corner in &mut uv {
                        for axis in corner {
                            *axis = r.f32()?;
                        }
                    }
                    quads.push(SourceQuad {
                        face,
                        tint,
                        layer,
                        sprite,
                        emission,
                        positions,
                        uv,
                    });
                }
                Model::Mesh(quads)
            }
            2 => {
                let count = r.count(8)?;
                let mut children = Vec::new();
                children.try_reserve_exact(count)?;
                let mut total = 0u32;
                for _ in 0..count {
                    let weight = r.u32()?;
                    let id = r.u32()?;
                    total = total
                        .checked_add(weight)
                        .ok_or(Error::Input("模型权重溢出"))?;
                    children.push((total, id));
                }
                if total == 0 || total > i32::MAX as u32 {
                    return Err(Error::Input("模型总权重无效"));
                }
                Model::Weighted { children, total }
            }
            3 => {
                let count = r.count(4)?;
                let mut children = Vec::new();
                children.try_reserve_exact(count)?;
                for _ in 0..count {
                    children.push(r.u32()?);
                }
                Model::Multipart(children)
            }
            4 => Model::Alias(r.u32()?),
            _ => return Err(Error::Input("未知模型拓扑种类")),
        };
        insert(&mut next.models, id, model)?;
    }
    for _ in 0..counts[2] {
        let id = r.u32()?;
        let nu = r.count(8)?;
        let mut u = Vec::new();
        u.try_reserve_exact(nu)?;
        for _ in 0..nu {
            u.push(r.f64()?);
        }
        let nv = r.count(8)?;
        let mut v = Vec::new();
        v.try_reserve_exact(nv)?;
        for _ in 0..nv {
            v.push(r.f64()?);
        }
        let count = r.count(8)?;
        if id < 2
            || nu < 2
            || nv < 2
            || u.windows(2).any(|p| p[0] >= p[1])
            || v.windows(2).any(|p| p[0] >= p[1])
            || nu
                .checked_sub(1)
                .and_then(|u| (nv - 1).checked_mul(u))
                .is_none_or(|bits| count != bits.div_ceil(64))
        {
            return Err(Error::Input("面占用坐标或位图布局无效"));
        }
        let mut words = Vec::new();
        words.try_reserve_exact(count)?;
        for _ in 0..count {
            words.push(r.u64()?);
        }
        insert(&mut next.faces, id, Face { u, v, words })?;
    }
    for _ in 0..counts[3] {
        let id = r.u32()?;
        let layer = r.u32()?;
        let flags = r.u32()?;
        let sprites = [r.u32()?, r.u32()?, r.u32()?];
        if id == 0 || layer > 2 || flags > 1 || sprites[0] == 0 || sprites[1] == 0 {
            return Err(Error::Input("流体材质源无效"));
        }
        insert(
            &mut next.fluids,
            id,
            Fluid {
                layer,
                tinted: flags != 0,
                sprites,
            },
        )?;
    }
    for _ in 0..counts[4] {
        let id = r.u32()?;
        let name = r.string()?;
        let bounds = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
        let extent = [r.u32()?, r.u32()?];
        let flags = r.u32()?;
        if id == 0
            || bounds[0] >= bounds[2]
            || bounds[1] >= bounds[3]
            || bounds.into_iter().any(|value| !(0. ..=1.).contains(&value))
            || extent.into_iter().any(|n| n == 0 || n > 16384)
            || flags > 3
        {
            return Err(Error::Input("sprite源描述无效"));
        }
        let count = r.count(8)?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(count)?;
        for _ in 0..count {
            let frame = [r.u32()?, r.u32()?];
            if frame[1] == 0 {
                return Err(Error::Input("动画帧时长不能为零"));
            }
            frames.push(frame);
        }
        let count = r.count(12)?;
        let mut images = Vec::new();
        images.try_reserve_exact(count)?;
        for _ in 0..count {
            images.push(image(&mut r, true)?);
        }
        let base = images
            .first()
            .ok_or(Error::Input("sprite缺少base图像布局"))?;
        let columns = base.width / extent[0];
        let rows = base.height / extent[1];
        if !base.width.is_multiple_of(extent[0])
            || !base.height.is_multiple_of(extent[1])
            || columns == 0
            || rows == 0
            || frames.iter().any(|frame| frame[0] >= columns * rows)
            || !frames.is_empty() && base.pixels.is_empty()
        {
            return Err(Error::Input("sprite动画帧与源图像布局不一致"));
        }
        let normal = optional_image(&mut r, base, extent, &frames, flags & 2 != 0)?;
        let specular = optional_image(&mut r, base, extent, &frames, flags & 2 != 0)?;
        insert(
            &mut next.sprites,
            id,
            SpriteSource {
                id,
                name,
                bounds,
                extent,
                vanilla: flags & 1 != 0,
                interpolate: flags & 2 != 0,
                frames,
                images,
                normal,
                specular,
            },
        )?;
    }
    if r.remaining() != 0 {
        let length = r.count(1)?;
        if length != 0 {
            let tints = crate::tint::TintResources::decode(r.take(length)?)?;
            if tints.generation() != generation {
                return Err(Error::Input("PTR1与模型资源世代不一致"));
            }
            next.tints = Some(tints);
        }
    }
    r.finish()?;
    Ok((next, flags != 0))
}

impl Catalog {
    /// 证明单状态压缩源是否为真实 `isAir`，不把不可见方块或无模型方块视为空。
    ///
    /// 非单状态布局返回 `false`；完整状态身份仍由接受边界的
    /// [`Self::validate_section`] 校验。单状态引用未准备 ID 时返回错误。
    pub fn section_is_air(&self, storage: &crate::SectionStorage) -> Result<bool, Error> {
        let Some(id) = storage.uniform_state() else {
            return Ok(false);
        };
        let state = self
            .states
            .get(&id)
            .ok_or(Error::Input("section引用未准备的状态ID"))?;
        Ok(state.flags & 1 != 0)
    }

    /// 接受压缩源前校验完整状态域，不追加或准备运行时资源。
    /// 局部调色板包含未使用项也全部校验；全局模式校验实际4096个registry ID。
    pub fn validate_section(&self, storage: &crate::SectionStorage) -> Result<(), Error> {
        let valid = if storage.palette_values().is_empty() {
            (0..4096).all(|index| {
                storage
                    .state(index)
                    .is_some_and(|id| self.states.contains_key(&id))
            })
        } else {
            storage
                .palette_values()
                .iter()
                .all(|id| self.states.contains_key(id))
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Input("section引用未准备的状态ID"))
        }
    }
    /// 当前成功资源世代；零表示尚未接受资源。
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// 借用实际sprite源，不复制像素；顺序不作为资源身份。
    pub fn sprites(&self) -> impl ExactSizeIterator<Item = &SpriteSource> {
        self.sprites.values()
    }
    /// 同资源世代唯一的原版tint recipe与颜色表；纯几何fixture可不包含该尾段。
    pub fn tints(&self) -> Option<&crate::tint::TintResources> {
        self.tints.as_ref()
    }

    /// Minecraft相机介质薄捕获：0空气、1水、2不透明岩浆；未知kind拒绝。
    /// 水使用与源边界相同的IOR及每米RGB消光；岩浆不声明可透射体介质，沿用空气路径。
    pub fn camera_medium(&self, kind: u32) -> Result<[f32; 4], Error> {
        match kind {
            0 | 2 => Ok([1., 0., 0., 0.]),
            1 => Ok([1.333, 0.2916, 0.04444, 0.010182]),
            _ => Err(Error::Input("相机介质kind无效")),
        }
    }

    /// 校验完整候选，不改变原字典语义；仅接受严格递增世代的完整替换。
    /// 材质与GPU准备在commit前完成，失败时丢弃候选即可保持旧所有者。
    pub fn stage_resources(&mut self, bytes: &[u8]) -> Result<PreparedCatalog<'_>, Error> {
        let (next, replace) = decode(bytes)?;
        if !replace || next.generation <= self.generation {
            return Err(Error::Input("资源世代发布顺序无效"));
        }
        let model = |id| next.models.get(&id);
        let face = |id| id <= 1 || next.faces.contains_key(&id);
        let sprite = |id| id == 0 || next.sprites.contains_key(&id);
        for state in next.states.values() {
            if state.model != 0 && model(state.model).is_none()
                || state.faces.iter().any(|id| !face(*id))
                || state.fluid != 0 && !next.fluids.contains_key(&state.fluid)
            {
                return Err(Error::Input("状态引用未定义资源"));
            }
        }
        for fluid in next.fluids.values() {
            if fluid.sprites.iter().any(|id| !sprite(*id)) {
                return Err(Error::Input("流体引用未定义sprite"));
            }
        }
        let mut completed = HashSet::new();
        let mut active = HashSet::new();
        let mut stack = Vec::new();
        for &root in next.models.keys() {
            stack.push((root, false));
            while let Some((id, exit)) = stack.pop() {
                if completed.contains(&id) {
                    continue;
                }
                if exit {
                    active.remove(&id);
                    completed.insert(id);
                    continue;
                }
                if !active.insert(id) {
                    return Err(Error::Input("模型资源拓扑含环"));
                }
                let value = model(id).ok_or(Error::Input("模型引用未定义子模型"))?;
                stack.push((id, true));
                match value {
                    Model::Mesh(quads) => {
                        if quads.iter().any(|q| !sprite(q.sprite)) {
                            return Err(Error::Input("quad引用未定义sprite"));
                        }
                    }
                    Model::Weighted { children, .. } => {
                        for &(_, id) in children.iter().rev() {
                            stack.push((id, false));
                        }
                    }
                    Model::Multipart(children) => {
                        for &id in children.iter().rev() {
                            stack.push((id, false));
                        }
                    }
                    Model::Alias(id) => stack.push((*id, false)),
                    Model::Unknown => {}
                }
            }
        }
        Ok(PreparedCatalog {
            catalog: self,
            next,
        })
    }
}
