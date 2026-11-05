//! 资源准备阶段构造共享数据atlas、材质身份、静态面积与纹理内alias。
//!
//! 原版 PBR 按 sprite 身份读取冻结数据。外部资源的 `_n`／`_s` 贴图当前只做检测和报告。
//! 动画包络用于准备保守材质能力，绘制使用准备时的首帧，材质 atlas 不随 tick 切换。
use crate::{
    Error, ImageSource, PreparedCatalog, SpriteSource,
    emission_power::{PowerMoments, textured_amplitude},
    resources::{BoundQuad, Model, ModelBinding, SourceQuad, State},
};
use prime_scene::{MaterialDefinition as Definition, MaterialId};
use simple_engine::light_sampling::{
    QuadArea,
    emitter::{Coverage, EmissionProposal, EmitterAlias, MAX_TEXELS},
};
use std::collections::{HashMap, HashSet};

/// 一次全局资源准备产物；CPU像素在GPU接受后释放，不按section复制。
pub struct MaterialResources<'a> {
    /// 候选拥有的完整共享材质定义，与CPU编译器MaterialId一致。
    pub materials: &'a [Definition],
    /// 与实际albedo相同的atlas宽高。
    pub extent: [u32; 2],
    /// 对齐albedo UV的原位线性MERS RGBA8。
    pub mers: Vec<u8>,
    /// 对齐albedo UV的normalXY/distribution/IOR RGBA8。
    pub auxiliary: Vec<u8>,
    /// 全局唯一静态alias words，word0保留为uniform fallback。
    pub emission_alias_words: Vec<u32>,
    /// 与materials等长的静态两三角面积，过程几何以0xffffffff为sentinel。
    pub quad_areas: Vec<[u32; 4]>,
}
struct SpriteMaps {
    authored: bool,
    mers: bool,
    auxiliary: bool,
    textured: bool,
    proposal: Option<EmissionProposal>,
    power: Option<PowerMoments>,
    glass_extinction: [f32; 3],
}

#[derive(Hash, PartialEq, Eq)]
struct EmissionKey {
    sprite: u32,
    uv: [[u32; 2]; 4],
    areas: [u64; 2],
    cutout: bool,
}

#[derive(Default)]
struct PreparedEmission {
    alias_offset: Option<u32>,
    power_ratio: Option<f64>,
}

fn glass_extinction(reference: [u8; 4]) -> [f32; 3] {
    if reference[3] == 0 {
        return [0.; 3];
    }
    std::array::from_fn(|channel| {
        let encoded = 0.6 + 0.4 * f32::from(reference[channel]) / 255.;
        let value = if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        };
        -value.clamp(1e-3, 1.).ln()
    })
}

fn exact_sum(a: f64, b: f64) -> (f64, f64) {
    let sum = a + b;
    let virtual_b = sum - a;
    (sum, (a - (sum - virtual_b)) + (b - virtual_b))
}

fn affine_quad<const N: usize>(corners: &[[f32; N]; 4]) -> bool {
    (0..N).all(|axis| {
        exact_sum(f64::from(corners[0][axis]), f64::from(corners[2][axis]))
            == exact_sum(f64::from(corners[1][axis]), f64::from(corners[3][axis]))
    })
}

fn bilateral(quads: &[SourceQuad]) -> (Vec<u8>, Vec<usize>) {
    let mut sides = vec![0; quads.len()];
    let mut skipped = Vec::new();
    for a in 0..quads.len() {
        if sides[a] != 0 {
            continue;
        }
        for b in a + 1..quads.len() {
            if sides[b] != 0 {
                continue;
            }
            let first = &quads[a];
            let second = &quads[b];
            let mapping = [0usize, 2, 1, 3].into_iter().find_map(|shift| {
                let map = std::array::from_fn::<_, 4, _>(|i| (shift + 4 - i) % 4);
                (shift % 2 == 0 || affine_quad(&first.positions)).then_some(())?;
                (0..4)
                    .all(|i| first.positions[i] == second.positions[map[i]])
                    .then_some((map, shift))
            });
            let Some((mapping, shift)) = mapping else {
                continue;
            };
            sides[a] = 1;
            sides[b] = 1;
            if shift % 2 == 0
                && first.face == second.face
                && first.tint == second.tint
                && first.layer == second.layer
                && first.sprite == second.sprite
                && first.emission == second.emission
                && (0..4).all(|i| first.uv[i] == second.uv[mapping[i]])
            {
                sides[a] = 3;
                sides[b] = 2;
                skipped.push(b);
            }
            break;
        }
    }
    (sides, skipped)
}

#[derive(Clone, Copy)]
enum GrassSide {
    Ordinary,
    Base { overlay: usize },
    Overlay,
}

fn sprite_local(uv: [f32; 2], bounds: [f32; 4]) -> [f32; 2] {
    std::array::from_fn(|axis| (uv[axis] - bounds[axis]) / (bounds[axis + 2] - bounds[axis]))
}

fn aligned_layers(
    base: &SourceQuad,
    bottom: [f32; 4],
    top: &SourceQuad,
    overlay: [f32; 4],
) -> bool {
    // 原版同面可循环重排顶点，保留绕序和每个对应顶点在各自sprite内的UV。
    base.face == top.face
        && (0..4).any(|shift| {
            // 奇数起点改变三角化对角线，须证明两组坐标仍为仿射插值。
            if shift % 2 != 0
                && !(affine_quad(&base.positions) && affine_quad(&base.uv) && affine_quad(&top.uv))
            {
                return false;
            }
            (0..4).all(|i| {
                let j = (i + shift) % 4;
                base.positions[i] == top.positions[j]
                    && sprite_local(base.uv[i], bottom)
                        .into_iter()
                        .zip(sprite_local(top.uv[j], overlay))
                        .all(|(a, b)| (a - b).abs() <= 1e-4)
            })
        })
}

fn grass_sides(
    quads: &[SourceQuad],
    sprites: &HashMap<u32, SpriteSource>,
    maps: &HashMap<u32, SpriteMaps>,
) -> Vec<GrassSide> {
    let mut sides = vec![GrassSide::Ordinary; quads.len()];
    for (index, base) in quads.iter().enumerate() {
        let bottom = &sprites[&base.sprite];
        let bottom_maps = &maps[&base.sprite];
        if base.tint >= 0
            || base.layer != 0
            || base.emission != 0
            || bottom_maps.textured
            || bottom_maps.auxiliary
            || bottom.name != "minecraft:block/grass_block_side"
        {
            continue;
        }
        if let Some(overlay) = quads.iter().enumerate().position(|(i, top)| {
            let sprite = &sprites[&top.sprite];
            let top_maps = &maps[&top.sprite];
            matches!(sides[i], GrassSide::Ordinary)
                && top.tint >= 0
                && top.layer == 1
                && top.emission == 0
                && !top_maps.textured
                && !top_maps.auxiliary
                && sprite.name == "minecraft:block/grass_block_side_overlay"
                && aligned_layers(base, bottom.bounds, top, sprite.bounds)
        }) {
            sides[index] = GrassSide::Base { overlay };
            sides[overlay] = GrassSide::Overlay;
        }
    }
    sides
}

fn linear(v: u8) -> f32 {
    let v = f32::from(v) / 255.;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn sample(image: &ImageSource, x: usize, y: usize, extent: [u32; 2], frame: usize) -> [u8; 4] {
    let columns = (image.width / extent[0]).max(1) as usize;
    let sx = (frame % columns * extent[0] as usize + x).min(image.width as usize - 1);
    let sy = (frame / columns * extent[1] as usize + y).min(image.height as usize - 1);
    image.pixels[(sy * image.width as usize + sx) * 4..][..4]
        .try_into()
        .expect("已验证RGBA8")
}
fn sample_material(
    image: &ImageSource,
    layout: crate::vanilla_frames::Layout,
    frame: u32,
    texel: [u32; 2],
    target: [u32; 2],
) -> [u8; 4] {
    let [x, y] = layout.source_texel(frame, texel, target);
    image.pixels[((y * image.width + x) * 4) as usize..][..4]
        .try_into()
        .expect("已验证内嵌RGBA8布局")
}
fn prepare_sprite(
    s: &SpriteSource,
    extent: [u32; 2],
    albedo: &[u8],
    mers: &mut [u8],
    aux: &mut [u8],
) -> Result<SpriteMaps, Error> {
    let entry = if s.vanilla {
        crate::vanilla::find(&s.name)
    } else {
        None
    };
    let images = entry.map(crate::vanilla::Entry::images).transpose()?;
    let material = images.as_ref().map(|p| &p.0);
    let auxiliary = images.as_ref().and_then(|p| p.1.as_ref());
    let layout = material
        .map(|image| crate::vanilla_frames::layout(&s.name, [image.width, image.height]))
        .transpose()?;
    let dx = (s.bounds[0] * extent[0] as f32).round() as usize;
    let dy = (s.bounds[1] * extent[1] as f32).round() as usize;
    let w = ((s.bounds[2] - s.bounds[0]) * extent[0] as f32).round() as usize;
    let h = ((s.bounds[3] - s.bounds[1]) * extent[1] as f32).round() as usize;
    if dx + w > extent[0] as usize || dy + h > extent[1] as usize || w == 0 || h == 0 {
        return Err(Error::Input("sprite atlas像素范围无效"));
    }
    let frame = layout
        .and_then(|layout| layout.frames().first())
        .map_or(0, |v| v[0]);
    for y in 0..h {
        for x in 0..w {
            let at = ((dy + y) * extent[0] as usize + dx + x) * 4;
            let point = [x as u32, y as u32];
            let target = [w as u32, h as u32];
            if let Some(i) = material {
                mers[at..at + 4].copy_from_slice(&sample_material(
                    i,
                    layout.expect("材质已建立布局"),
                    frame,
                    point,
                    target,
                ));
            }
            if let Some(i) = auxiliary {
                aux[at..at + 4].copy_from_slice(&sample_material(
                    i,
                    layout.expect("辅助图与材质同布局"),
                    frame,
                    point,
                    target,
                ));
            }
        }
    }
    let textured = material.is_some_and(|m| m.pixels.as_chunks::<4>().0.iter().any(|p| p[1] != 0));
    let source_frames = if s.frames.is_empty() {
        &[[0, 1]][..]
    } else {
        &s.frames
    };
    let count = s.extent[0] as usize * s.extent[1] as usize;
    let keep_proposal = count <= MAX_TEXELS;
    let mut proposal_samples = keep_proposal.then(|| (vec![0f32; count], vec![0u8; count]));
    let mut color_texel = |[x, y]: [u32; 2], frame: u32| {
        let [x, y] = [x as usize, y as usize];
        let rgba = if let Some(i) = s.images.first().filter(|i| !i.pixels.is_empty()) {
            sample(i, x, y, s.extent, frame as usize)
        } else {
            let sx = dx + x * w / s.extent[0] as usize;
            let sy = dy + y * h / s.extent[1] as usize;
            albedo[(sy * extent[0] as usize + sx) * 4..][..4]
                .try_into()
                .expect("已验证atlas")
        };
        let luminance = color_luminance(rgba);
        if let Some((weights, alpha)) = &mut proposal_samples {
            let at = y * s.extent[0] as usize + x;
            weights[at] = weights[at].max(luminance);
            alpha[at] = alpha[at].max(rgba[3]);
        }
        (luminance, rgba[3])
    };
    let power = if textured {
        let material = material.expect("textured来自实际材质图");
        let layout = layout.expect("材质已建立布局");
        // 同一宿主帧内的 Color 与 G 配对，再以声明时长积分。冻结 GPU 帧独立于参考功率。
        let mut durations = std::collections::BTreeMap::<u32, u64>::new();
        for &[host_frame, duration] in source_frames {
            *durations.entry(host_frame).or_default() += u64::from(duration);
        }
        Some(PowerMoments::new(s.extent, |texel| {
            let mut moments = [0.; 4];
            for (&host_frame, &duration) in &durations {
                let (luminance, alpha) = color_texel(texel, host_frame);
                let energy = f64::from(luminance) * duration as f64;
                let material_frame = if s.frames.is_empty() {
                    frame
                } else {
                    host_frame
                };
                let g = f64::from(
                    sample_material(material, layout, material_frame, texel, s.extent)[1],
                ) / 255.;
                moments[0] += energy;
                moments[1] += energy * g;
                if alpha >= 128 {
                    moments[2] += energy;
                    moments[3] += energy * g;
                }
            }
            moments
        })?)
    } else {
        if keep_proposal {
            for &[frame, _] in source_frames {
                for y in 0..s.extent[1] {
                    for x in 0..s.extent[0] {
                        color_texel([x, y], frame);
                    }
                }
            }
        }
        None
    };
    let proposal = if let Some((mut weights, alpha)) = proposal_samples {
        if textured {
            let mut emission_envelope = vec![0f32; count];
            let material = material.expect("textured来自实际材质图");
            let layout = layout.expect("材质已建立布局");
            let frames = if layout.frames().is_empty() {
                &[[0, 1]][..]
            } else {
                layout.frames()
            };
            for &[frame, _] in frames {
                for y in 0..s.extent[1] {
                    for x in 0..s.extent[0] {
                        let at = (y * s.extent[0] + x) as usize;
                        let emission = f32::from(
                            sample_material(material, layout, frame, [x, y], s.extent)[1],
                        ) / 255.;
                        emission_envelope[at] = emission_envelope[at].max(emission);
                    }
                }
            }
            for (weight, emission) in weights.iter_mut().zip(emission_envelope) {
                *weight *= emission;
            }
        }
        EmissionProposal::new(s.extent, &weights, &alpha)?
    } else {
        None
    };
    Ok(SpriteMaps {
        authored: entry.is_some_and(|e| e.authored_normal),
        mers: material.is_some(),
        auxiliary: auxiliary.is_some(),
        textured,
        proposal,
        power,
        glass_extinction: glass_extinction(
            albedo[((dy + h / 2) * extent[0] as usize + dx + w / 2) * 4..][..4]
                .try_into()
                .expect("已验证atlas中点"),
        ),
    })
}

fn color_luminance([r, g, b, _]: [u8; 4]) -> f32 {
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn native_amplitude(level: u32) -> f32 {
    (level as f32 / 15.).powi(2)
}

fn definition(state: &State, q: &SourceQuad, s: &SpriteSource, m: &SpriteMaps) -> Definition {
    let mut v = Definition {
        uv_bounds: s.bounds,
        ..Definition::default()
    };
    v.control[2] = match q.layer {
        1 => Definition::CUTOUT,
        2 => Definition::TRANSLUCENT,
        _ => 0,
    };
    if m.mers {
        v.control[2] |= Definition::MERS;
    }
    if m.auxiliary {
        v.control[2] |= Definition::AUXILIARY;
    }
    v.control[3] = u32::from(m.authored);
    if m.textured {
        v.control[2] |= Definition::TEXTURED_EMISSION;
        v.emission = [1.; 4];
    } else if state.emission.max(q.emission) > 0 {
        v.emission = [1., 1., 1., native_amplitude(state.emission.max(q.emission))];
    }
    if q.layer == 1 {
        v.control[1] = 3;
    }
    if state.fluid_kind == 1 {
        v.control[0] = 1;
        v.surface = [1.333, 0., 0., 0.];
        v.extinction = [1.333, 0.2916, 0.04444, 0.010182];
        v.base_roughness[3] = 0.;
    } else if state.flags & 768 != 0 {
        v.control[0] = if state.flags & 512 != 0 { 3 } else { 1 };
        v.surface = [1.5, 0., 0., if state.flags & 512 != 0 { 0.01 } else { 0. }];
        v.extinction = [
            1.5,
            m.glass_extinction[0],
            m.glass_extinction[1],
            m.glass_extinction[2],
        ];
        v.base_roughness[3] = 0.;
    }
    v
}

impl PreparedCatalog<'_> {
    /// 候选世代唯一材质准备；albedo仅借用，完成后不保留重复颜色atlas。
    /// 每sprite一次动画包络；重复UV/面积/覆盖组合复用裁剪结果及静态表。
    pub fn material_resources(
        &mut self,
        extent: [u32; 2],
        albedo: &[u8],
    ) -> Result<MaterialResources<'_>, Error> {
        if extent.contains(&0)
            || u64::from(extent[0]) * u64::from(extent[1]) * 4 != albedo.len() as u64
        {
            return Err(Error::Input("材质准备albedo atlas布局无效"));
        }
        if !self.next.definitions.is_empty() {
            return Err(Error::Input("同一候选不得重复准备材质"));
        }
        let mut mers = [0, 0, 242, 0].repeat(albedo.len() / 4);
        let mut auxiliary = [128, 128, 0, 85].repeat(albedo.len() / 4);
        let mut maps = HashMap::new();
        maps.try_reserve(self.next.sprites.len())?;
        for s in self.next.sprites.values() {
            maps.insert(
                s.id,
                prepare_sprite(s, extent, albedo, &mut mers, &mut auxiliary)?,
            );
        }
        let mut definitions = vec![
            Definition::default(),
            Definition {
                base_roughness: [1., 1., 1., 0.25],
                ..Definition::default()
            },
        ];
        let sentinel = [u32::MAX, 0, 0, 0];
        let mut areas = vec![sentinel; 2];
        let mut intern = HashMap::new();
        for (id, v) in definitions.iter().enumerate() {
            intern.insert((v.words(), sentinel), MaterialId::new(id as u32));
        }
        let mut words = vec![0u32];
        let mut word_intern: HashMap<Vec<u32>, u32> = HashMap::new();
        let mut geometry_intern: HashMap<EmissionKey, PreparedEmission> = HashMap::new();
        let mut sides = HashMap::new();
        for (&id, model) in &self.next.models {
            if let Model::Mesh(quads) = model {
                let (flags, skip) = bilateral(quads);
                for index in skip {
                    self.next.skipped_quads.insert((id, index));
                }
                sides.insert(id, flags);
            }
        }
        let mut ids: Vec<_> = self.next.states.keys().copied().collect();
        ids.sort_unstable();
        let mut matching_blocks = HashMap::new();
        for state_id in ids {
            let state = &self.next.states[&state_id];
            let mut stack = vec![state.model];
            let mut seen = HashSet::new();
            let mut bindings = Vec::new();
            let mut face_mask = 0;
            while let Some(model_id) = stack.pop() {
                if model_id == 0 || !seen.insert(model_id) {
                    continue;
                }
                match &self.next.models[&model_id] {
                    Model::Unknown => {
                        face_mask |= 127;
                    }
                    Model::Alias(id) => stack.push(*id),
                    Model::Weighted { children, .. } => {
                        for &(_, id) in children.iter().rev() {
                            stack.push(id);
                        }
                    }
                    Model::Multipart(children) => {
                        for &id in children.iter().rev() {
                            stack.push(id);
                        }
                    }
                    Model::Mesh(quads) => {
                        let grass = (state.name == "minecraft:grass_block"
                            && state.emission == 0
                            && state.fluid_kind == 0
                            && state.flags & 768 == 0)
                            .then(|| grass_sides(quads, &self.next.sprites, &maps));
                        let mut bound = Vec::with_capacity(quads.len());
                        for (index, q) in quads.iter().enumerate() {
                            let grass = grass
                                .as_ref()
                                .map_or(GrassSide::Ordinary, |sides| sides[index]);
                            if self.next.skipped_quads.contains(&(model_id, index))
                                || matches!(grass, GrassSide::Overlay)
                            {
                                continue;
                            }
                            let s = &self.next.sprites[&q.sprite];
                            let m = &maps[&q.sprite];
                            let mut v = definition(state, q, s, m);
                            let tint = match grass {
                                GrassSide::Base { overlay } => {
                                    let top = &quads[overlay];
                                    v.control[2] |= Definition::OVERLAY;
                                    if maps[&top.sprite].mers {
                                        v.control[2] |= Definition::OVERLAY_MERS;
                                    }
                                    v.overlay_bounds = self.next.sprites[&top.sprite].bounds;
                                    top.tint
                                }
                                _ => q.tint,
                            };
                            let mut area = sentinel;
                            match sides[&model_id][index] {
                                1 => {
                                    v.control[2] |= Definition::FRONT_ONLY;
                                    v.control[1] = 1;
                                }
                                3 => {
                                    v.control[2] |= Definition::TWO_SIDED;
                                    v.control[1] = 3;
                                }
                                _ => {}
                            }
                            if v.emission[3] > 0.
                                && let Ok(geometry) = QuadArea::new(&q.positions)
                            {
                                area = geometry.words();
                                let level = state.emission.max(q.emission);
                                let power = m.power.as_ref().filter(|_| level > 0);
                                if m.proposal.is_some() || power.is_some() {
                                    let uv = q.uv.map(|uv| {
                                        [
                                            (uv[0] - s.bounds[0]) / (s.bounds[2] - s.bounds[0]),
                                            (uv[1] - s.bounds[1]) / (s.bounds[3] - s.bounds[1]),
                                        ]
                                    });
                                    let cutout = q.layer == 1;
                                    let key = EmissionKey {
                                        sprite: q.sprite,
                                        uv: uv.map(|uv| uv.map(f32::to_bits)),
                                        areas: geometry.areas().map(f64::to_bits),
                                        cutout,
                                    };
                                    let prepared = geometry_intern.entry(key).or_default();
                                    if let Some(power) = power {
                                        let ratio =
                                            *prepared.power_ratio.get_or_insert_with(|| {
                                                power.ratio(Some((uv, geometry.areas())), cutout)
                                            });
                                        v.emission[3] =
                                            textured_amplitude(native_amplitude(level), ratio);
                                    }
                                    if let Some(proposal) = &m.proposal {
                                        let offset = if let Some(offset) = prepared.alias_offset {
                                            offset
                                        } else {
                                            let coverage = if cutout {
                                                Coverage::AlphaThreshold(128)
                                            } else {
                                                Coverage::Opaque
                                            };
                                            let offset = if let Some(table) = EmitterAlias::build(
                                                q.positions,
                                                uv,
                                                coverage,
                                                proposal,
                                            )? {
                                                if let Some(offset) = word_intern.get(table.words())
                                                {
                                                    *offset
                                                } else {
                                                    let offset = u32::try_from(words.len())
                                                        .map_err(|_| {
                                                            Error::Input("资源alias偏移溢出")
                                                        })?;
                                                    if offset > u32::MAX >> 1 {
                                                        return Err(Error::Input(
                                                            "资源alias偏移超过31bit",
                                                        ));
                                                    }
                                                    words.extend_from_slice(table.words());
                                                    word_intern
                                                        .insert(table.words().to_vec(), offset);
                                                    offset
                                                }
                                            } else {
                                                0
                                            };
                                            prepared.alias_offset = Some(offset);
                                            offset
                                        };
                                        v.control[3] |= offset << 1;
                                    }
                                }
                            }
                            let key = (v.words(), area);
                            let id = if let Some(id) = intern.get(&key) {
                                *id
                            } else {
                                let id = MaterialId::new(
                                    u32::try_from(definitions.len())
                                        .map_err(|_| Error::Input("材质表索引溢出"))?,
                                );
                                definitions.push(v);
                                areas.push(area);
                                intern.insert(key, id);
                                id
                            };
                            bound.push(BoundQuad {
                                index,
                                material: id,
                                tint: if tint < 0 {
                                    None
                                } else {
                                    Some(
                                        self.next
                                            .tints()
                                            .ok_or(Error::Input("模型tint缺少准备资源"))?
                                            .source_id(state_id, tint)?,
                                    )
                                },
                            });
                            face_mask |= 1 << q.face;
                        }
                        bindings.push(ModelBinding {
                            model: model_id,
                            quads: bound,
                        });
                    }
                }
            }
            if state.fluid_kind != 0 {
                let f = self
                    .next
                    .fluids
                    .get(&state.fluid)
                    .ok_or(Error::Input("未准备的流体资源"))?;
                for which in 0..3 {
                    let sprite = f.sprites[which];
                    if sprite == 0 {
                        continue;
                    }
                    let q = SourceQuad {
                        face: 6,
                        tint: -1,
                        layer: f.layer,
                        sprite,
                        emission: state.emission,
                        positions: [[0.; 3]; 4],
                        uv: [[0.; 2]; 4],
                    };
                    let maps = &maps[&sprite];
                    let mut v = definition(state, &q, &self.next.sprites[&sprite], maps);
                    if let Some(power) = &maps.power
                        && state.emission > 0
                    {
                        let ratio = power.ratio(None, q.layer == 1);
                        v.emission[3] = textured_amplitude(native_amplitude(state.emission), ratio);
                    }
                    let key = (v.words(), sentinel);
                    let id = if let Some(id) = intern.get(&key) {
                        *id
                    } else {
                        let id = MaterialId::new(definitions.len() as u32);
                        definitions.push(v);
                        areas.push(sentinel);
                        intern.insert(key, id);
                        id
                    };
                    self.next.fluid_materials.insert((state_id, which), id);
                }
            }
            let matching = state.name.ends_with("glass")
                || matches!(state.name.as_str(), "minecraft:water" | "minecraft:lava")
                || state.flags & 128 != 0
                    && matches!(
                        state.name.as_str(),
                        "minecraft:ice" | "minecraft:frosted_ice"
                    );
            let next_matching = matching_blocks.len() as u32 + 1;
            let matching_block = if matching {
                *matching_blocks
                    .entry(state.name.clone())
                    .or_insert(next_matching)
            } else {
                0
            };
            let blocks_motion = state.flags & 32 != 0
                && !matches!(
                    state.name.as_str(),
                    "minecraft:cobweb" | "minecraft:bamboo_sapling"
                );
            let fluid_tint = if state.fluid_kind != 0 && self.next.fluids[&state.fluid].tinted {
                Some(
                    self.next
                        .tints()
                        .ok_or(Error::Input("流体tint缺少准备资源"))?
                        .source_id(state_id, -1)?,
                )
            } else {
                None
            };
            bindings.sort_unstable_by_key(|binding| binding.model);
            let state = self
                .next
                .states
                .get_mut(&state_id)
                .expect("候选状态仍在原表");
            state.leaves = bindings;
            state.face_mask = face_mask;
            state.matching_block = matching_block;
            state.blocks_motion = blocks_motion;
            state.fluid_tint = fluid_tint;
        }
        self.next.definitions = definitions;
        Ok(MaterialResources {
            materials: &self.next.definitions,
            extent,
            mers,
            auxiliary,
            emission_alias_words: words,
            quad_areas: areas,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_emitter_preserves_dark_texels_and_mixes_authored_and_host_power() -> Result<(), Error> {
        let mut catalog = crate::Catalog::default();
        let mut prepared = catalog.stage_resources(&crate::fixtures::resource_packet())?;
        prepared.next.states.get_mut(&1).unwrap().emission = 15;
        let sprite = prepared.next.sprites.get_mut(&1).unwrap();
        sprite.name = "minecraft:block/torch".into();
        sprite.vanilla = true;
        let resources = prepared.material_resources([32, 16], &[255; 32 * 16 * 4])?;
        let green: Vec<_> = resources
            .mers
            .as_chunks::<{ 32 * 4 }>()
            .0
            .iter()
            .flat_map(|row| row[..16 * 4].as_chunks::<4>().0)
            .map(|p| f64::from(p[1]) / 255.)
            .collect();
        let authored_mean = green.iter().sum::<f64>() / green.len() as f64;
        let expected_mean = (1. + authored_mean) * 0.5;
        let state = &prepared.next.states[&1];
        let material =
            prepared.next.definitions[state.leaves[0].quads[0].material.index() as usize];
        let amplitude = f64::from(material.emission[3]);
        assert_ne!(material.control[2] & Definition::TEXTURED_EMISSION, 0);
        assert!(green.contains(&0.));
        assert!((authored_mean * amplitude - expected_mean).abs() < 1e-7);
        Ok(())
    }

    #[test]
    fn grass_sides_bind_only_one_quad_with_the_overlay_tint() -> Result<(), Error> {
        let mut catalog = crate::Catalog::default();
        let mut prepared = catalog.stage_resources(&crate::fixtures::resource_packet())?;
        prepared.next.states.get_mut(&2).unwrap().name = "minecraft:grass_block".into();
        for (id, name) in [(1, "grass_block_side"), (2, "grass_block_side_overlay")] {
            let sprite = prepared.next.sprites.get_mut(&id).unwrap();
            sprite.name = format!("minecraft:block/{name}");
            sprite.vanilla = true;
        }
        let Model::Mesh(quads) = prepared.next.models.get_mut(&2).unwrap() else {
            unreachable!("固定测试模型")
        };
        let base = std::mem::take(quads);
        for mut q in base {
            if (2..6).contains(&q.face) {
                // 四个侧面与不同顶点起点，覆盖层先于底层。
                let first = q.face as usize % 4;
                quads.push(SourceQuad {
                    positions: std::array::from_fn(|i| q.positions[(i + first) % 4]),
                    uv: std::array::from_fn(|i| {
                        let [u, v] = q.uv[(i + first) % 4];
                        [u + 0.5, v]
                    }),
                    sprite: 2,
                    ..q
                });
                q.layer = 0;
                q.tint = -1;
            }
            quads.push(q);
        }
        prepared.material_resources([32, 16], &[255; 32 * 16 * 4])?;
        let state = &prepared.next.states[&2];
        let bound = &state.leaves[0].quads;
        assert_eq!(bound.len(), 6);
        let Model::Mesh(quads) = &prepared.next.models[&2] else {
            unreachable!("固定测试模型")
        };
        for face in bound
            .iter()
            .filter(|binding| quads[binding.index].face >= 2)
        {
            let material = prepared.next.definitions[face.material.index() as usize];
            assert_eq!(quads[face.index].sprite, 1);
            assert_eq!(
                face.tint,
                Some(prepared.next.tints().unwrap().source_id(2, 0)?)
            );
            assert_eq!(
                material.control[2],
                Definition::MERS | Definition::OVERLAY | Definition::OVERLAY_MERS
            );
            assert_eq!(material.overlay_bounds, [0.5, 0., 1., 1.]);
        }
        Ok(())
    }

    #[test]
    fn glass_midpoint_transparency_and_color_define_bt709_absorption() {
        assert_eq!(glass_extinction([18, 45, 197, 0]), [0.; 3]);
        assert_eq!(glass_extinction([255, 255, 255, 128]), [0.; 3]);
        let red = glass_extinction([255, 0, 64, 128]);
        assert_eq!(red[0], 0.);
        assert!(red[1] > red[2] && red[2] > 0.);
        let expected = -((0.6f32 + 0.055) / 1.055).powf(2.4).ln();
        assert!((red[1] - expected).abs() < 1e-6);
    }

    #[test]
    fn bilateral_pairs_preserve_distinct_uv_and_merge_only_identical_attributes() {
        let a = SourceQuad {
            face: 6,
            tint: -1,
            layer: 1,
            sprite: 1,
            emission: 0,
            positions: [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
            uv: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
        };
        let reverse = |different: bool| SourceQuad {
            face: 6,
            tint: -1,
            layer: 1,
            sprite: 1,
            emission: 0,
            positions: [
                a.positions[0],
                a.positions[3],
                a.positions[2],
                a.positions[1],
            ],
            uv: if different {
                a.uv
            } else {
                [a.uv[0], a.uv[3], a.uv[2], a.uv[1]]
            },
        };
        let (sides, skipped) = bilateral(&[SourceQuad { ..a }, reverse(true)]);
        assert_eq!(sides, vec![1, 1]);
        assert!(skipped.is_empty());
        let b = reverse(false);
        let (sides, skip) = bilateral(&[a, b]);
        assert_eq!(sides, vec![3, 2]);
        assert_eq!(skip, vec![1]);
    }
}
