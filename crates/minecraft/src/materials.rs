//! 资源准备阶段构造共享数据atlas、材质身份、静态面积与纹理内alias。
//!
//! 优先使用当前资源包栈提供的 LabPBR 伴随图。纯原版 sprite 可使用冻结预设。
//! 动画包络用于准备保守材质能力，绘制使用准备时的首帧，材质 atlas 不随 tick 切换。
use crate::{
    Error, ImageSource, PreparedCatalog, SpriteSource,
    emission_power::{PowerMoments, textured_amplitude},
    material_model::{ModelFaces, Side, affine_quad, bilateral},
    resources::{BoundQuad, ModelBinding, SourceQuad, State},
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
    /// 可移动物品的表面发光索引，与发光数据中的逐纹素遮罩同世代。
    pub item_materials: prime_scene::ItemMaterials,
    /// 与materials等长的静态两三角面积，过程几何以0xffffffff为sentinel。
    pub quad_areas: Vec<[u32; 4]>,
    /// 资源期收集的唯一 cutout quad UV；渲染器仅对精确对齐者准备 OMM。
    pub opacity_quads: Vec<simple_engine::opacity::Quad>,
}
struct SpriteMaps {
    authored: bool,
    mers: bool,
    auxiliary: bool,
    lab_specular: bool,
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
    layout: crate::vanilla_frames::Layout<'_>,
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
    let external = s.normal.is_some() || s.specular.is_some();
    let entry = if s.vanilla && !external {
        crate::vanilla::find(&s.name)
    } else {
        None
    };
    let images = entry.map(crate::vanilla::Entry::images).transpose()?;
    let material = s
        .specular
        .as_ref()
        .map(|m| &m.image)
        .or_else(|| images.as_ref().map(|p| &p.0));
    let auxiliary = s
        .normal
        .as_ref()
        .map(|m| &m.image)
        .or_else(|| images.as_ref().and_then(|p| p.1.as_ref()));
    let layout = if let Some(map) = &s.specular {
        Some(map.layout())
    } else {
        material
            .map(|image| crate::vanilla_frames::layout(&s.name, [image.width, image.height]))
            .transpose()?
    };
    let normal_layout = s.normal.as_ref().map(|m| m.layout()).or(layout);
    let normal_frame = normal_layout
        .and_then(|l| l.frames().first())
        .map_or(0, |v| v[0]);
    let lab_specular = s.specular.is_some();
    let emission = |p: [u8; 4]| {
        if lab_specular {
            crate::lab_pbr::emission(p[3])
        } else {
            p[1]
        }
    };
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
                let texel =
                    sample_material(i, layout.expect("材质已建立布局"), frame, point, target);
                mers[at..at + 4].copy_from_slice(&if lab_specular {
                    crate::lab_pbr::mers(texel)
                } else {
                    texel
                });
                if lab_specular {
                    aux[at + 3] = texel[1];
                }
            }
            if let Some(i) = auxiliary {
                let texel = sample_material(
                    i,
                    normal_layout.expect("辅助图已建立布局"),
                    normal_frame,
                    point,
                    target,
                );
                if external {
                    aux[at..at + 2].copy_from_slice(&texel[..2]);
                } else {
                    aux[at..at + 4].copy_from_slice(&texel);
                }
            }
        }
    }
    let textured = material.is_some_and(|m| {
        m.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|&p| emission(p) != 0)
    });
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
                let g = f64::from(emission(sample_material(
                    material,
                    layout,
                    material_frame,
                    texel,
                    s.extent,
                ))) / 255.;
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
                        let emission = f32::from(emission(sample_material(
                            material,
                            layout,
                            frame,
                            [x, y],
                            s.extent,
                        ))) / 255.;
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
        authored: s.normal.is_some() || entry.is_some_and(|e| e.authored_normal),
        mers: material.is_some(),
        auxiliary: auxiliary.is_some() || lab_specular,
        lab_specular,
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
    prime_scene::ItemMaterials::native_amplitude(level)
}

fn append_item_mask(
    items: &mut prime_scene::ItemMaterials,
    words: &mut Vec<u32>,
    sprite: &SpriteSource,
    size: [u32; 2],
    ratios: [f64; 2],
    sample: impl Fn([u32; 2]) -> u8,
) -> Result<(), Error> {
    let [w, h] = size;
    let word = u32::try_from(words.len()).map_err(|_| Error::Input("发光数据偏移溢出"))?;
    let len = (w * h).div_ceil(4) as usize;
    if words.len() + 5 + len > u32::MAX as usize / 4 {
        return Err(Error::Input("物品发光数据超过设备寻址范围"));
    }
    words.try_reserve(5 + len)?;
    words.extend(sprite.bounds.map(f32::to_bits));
    words.push(w | h << 16);
    let first = words.len();
    words.resize(first + len, 0);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            words[first + i / 4] |= u32::from(sample([x, y])) << ((i % 4) * 8);
        }
    }
    let gains = ratios.map(|ratio| {
        std::array::from_fn(|level| {
            if level == 0 {
                1.
            } else {
                textured_amplitude(native_amplitude(level as u32), ratio)
            }
        })
    });
    items
        .masks
        .insert(sprite.id, prime_scene::ItemEmissionMask { word, gains });
    Ok(())
}

enum SurfaceSource {
    Model,
    Fluid,
}

fn definition(
    source: SurfaceSource,
    state: &State,
    q: &SourceQuad,
    s: &SpriteSource,
    m: &SpriteMaps,
    mapped_block: bool,
) -> Definition {
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
    if m.lab_specular {
        v.control[2] |= Definition::LAB_SPECULAR;
    }
    v.control[3] = u32::from(m.authored);
    if m.textured {
        v.control[2] |= Definition::TEXTURED_EMISSION;
        v.emission = [1.; 4];
    } else if !mapped_block && state.emission.max(q.emission) > 0 {
        v.emission = [1., 1., 1., native_amplitude(state.emission.max(q.emission))];
    }
    if q.layer == 1 {
        v.control[1] = 3;
    }
    // 含水状态同时拥有模型面与流体面，只有流体几何使用水的介电界面。
    if matches!(source, SurfaceSource::Fluid) && state.fluid_kind == 1 {
        v.control[0] = 1;
        v.surface = [1.333, 0., 0., 0.];
        v.extinction = [1.333, 0.2916, 0.04444, 0.010182];
        v.base_roughness[3] = 0.;
    } else if matches!(source, SurfaceSource::Model) && state.flags & 768 != 0 {
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
        let mut opacity_quads = Vec::new();
        let mut opacity_keys = HashSet::new();
        let mut intern = HashMap::new();
        for (id, v) in definitions.iter().enumerate() {
            intern.insert((v.words(), sentinel), MaterialId::new(id as u32));
        }
        let mut words = vec![0u32];
        let mut word_intern: HashMap<Vec<u32>, u32> = HashMap::new();
        let mut geometry_intern: HashMap<EmissionKey, PreparedEmission> = HashMap::new();
        let mut roots = HashMap::new();
        let mut masked_blocks = HashSet::new();
        // 模型、加权变体和流体可被多个方块共享。发光遮罩的优先级属于方块自身。
        for state in self.next.states.values() {
            let faces = roots.entry(state.model).or_insert_with(|| {
                ModelFaces::collect(state.model, &self.next.models, |sprite| {
                    maps[&sprite].textured
                })
            });
            let fluid_emits = state.fluid_kind != 0
                && self.next.fluids.get(&state.fluid).is_some_and(|fluid| {
                    fluid
                        .sprites
                        .iter()
                        .any(|s| maps.get(s).is_some_and(|m| m.textured))
                });
            if (faces.mapped_emission || fluid_emits) && !masked_blocks.contains(&state.name) {
                masked_blocks.insert(state.name.clone());
            }
        }
        let mut item_materials = prime_scene::ItemMaterials::default();
        let mut sides = HashMap::new();
        let mut ids: Vec<_> = self.next.states.keys().copied().collect();
        ids.sort_unstable();
        let mut matching_blocks = HashMap::new();
        for state_id in ids {
            let state = &self.next.states[&state_id];
            let faces = &roots[&state.model];
            let mapped_block = masked_blocks.contains(&state.name);
            item_materials
                .states
                .insert(state_id, (state.emission as u8, mapped_block));
            let attached = state.flags & State::MULTIFACE != 0;
            let mut bindings = Vec::with_capacity(faces.meshes.len());
            let mut face_mask = if faces.unknown { 127 } else { 0 };
            for &(model_id, quads) in &faces.meshes {
                let sides = sides
                    .entry((model_id, attached))
                    .or_insert_with(|| bilateral(quads, attached));
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
                    if sides[index] == Side::Omit || matches!(grass, GrassSide::Overlay) {
                        continue;
                    }
                    let s = &self.next.sprites[&q.sprite];
                    let m = &maps[&q.sprite];
                    let mut v = definition(SurfaceSource::Model, state, q, s, m, mapped_block);
                    let tint = match grass {
                        GrassSide::Base { overlay } => {
                            let top = &quads[overlay];
                            v.control[2] |= Definition::OVERLAY;
                            let maps = &maps[&top.sprite];
                            v.control[2] |= u32::from(maps.mers) * Definition::OVERLAY_MERS
                                | u32::from(maps.auxiliary) * Definition::OVERLAY_AUXILIARY
                                | u32::from(maps.authored) * Definition::OVERLAY_NORMAL
                                | u32::from(maps.lab_specular) * Definition::OVERLAY_LAB_SPECULAR;
                            v.overlay_bounds = self.next.sprites[&top.sprite].bounds;
                            top.tint
                        }
                        _ => q.tint,
                    };
                    let mut area = sentinel;
                    match sides[index] {
                        Side::Front => {
                            v.control[2] |= Definition::FRONT_ONLY;
                            v.control[1] = 1;
                        }
                        Side::Both => {
                            v.control[2] |= Definition::TWO_SIDED;
                            v.control[1] = 3;
                        }
                        Side::Single if faces.planar => v.control[2] |= Definition::PLANAR_SHEET,
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
                                let ratio = *prepared.power_ratio.get_or_insert_with(|| {
                                    power.ratio(Some((uv, geometry.areas())), cutout)
                                });
                                v.emission[3] = textured_amplitude(native_amplitude(level), ratio);
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
                                    let offset = if let Some(table) =
                                        EmitterAlias::build(q.positions, uv, coverage, proposal)?
                                    {
                                        if let Some(offset) = word_intern.get(table.words()) {
                                            *offset
                                        } else {
                                            let offset = u32::try_from(words.len())
                                                .map_err(|_| Error::Input("资源alias偏移溢出"))?;
                                            if offset > u32::MAX >> 1 {
                                                return Err(Error::Input("资源alias偏移超过31bit"));
                                            }
                                            words.extend_from_slice(table.words());
                                            word_intern.insert(table.words().to_vec(), offset);
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
                    if v.control[2] & (Definition::CUTOUT | Definition::FRONT_ONLY)
                        == Definition::CUTOUT
                    {
                        let key = (id.index(), q.uv.map(|p| p.map(f32::to_bits)));
                        if !opacity_keys.contains(&key) {
                            opacity_keys.try_reserve(1)?;
                            opacity_quads.try_reserve(1)?;
                            opacity_keys.insert(key);
                            opacity_quads.push(simple_engine::opacity::Quad {
                                material: id.index(),
                                uv: q.uv,
                            });
                        }
                    }
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
                    let mut v = definition(
                        SurfaceSource::Fluid,
                        state,
                        &q,
                        &self.next.sprites[&sprite],
                        maps,
                        mapped_block,
                    );
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
        // 地形复用已解码的 MERS 与光功率。物品图集仅保存真正发光的 R8 子图。
        for s in self.next.sprites.values() {
            let m = &maps[&s.id];
            if let Some(power) = &m.power {
                let ratios = [power.ratio(None, false), power.ratio(None, true)];
                // 零尺寸头引用原有方块 MERS 图集，避免重复上传已准备的纹素。
                append_item_mask(&mut item_materials, &mut words, s, [0; 2], ratios, |_| 0)?;
            }
        }
        for (_, s) in std::mem::take(&mut self.next.item_sprites) {
            let Some(spec) = &s.specular else { continue };
            if !spec
                .image
                .pixels
                .chunks_exact(4)
                .any(|p| crate::lab_pbr::emission(p[3]) != 0)
            {
                continue;
            }
            let layout = spec.layout();
            let frame = layout.frames().first().map_or(0, |f| f[0]);
            append_item_mask(
                &mut item_materials,
                &mut words,
                &s,
                s.extent,
                [1.; 2],
                |p| {
                    crate::lab_pbr::emission(
                        sample_material(&spec.image, layout, frame, p, s.extent)[3],
                    )
                },
            )?;
        }
        self.next.definitions = definitions;
        Ok(MaterialResources {
            materials: &self.next.definitions,
            extent,
            mers,
            auxiliary,
            emission_alias_words: words,
            item_materials,
            quad_areas: areas,
            opacity_quads,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Model;

    #[test]
    fn waterlogged_model_keeps_its_material_separate_from_fluid() -> Result<(), Error> {
        for glass in [false, true] {
            let mut catalog = crate::Catalog::default();
            let mut prepared = catalog.stage_resources(&crate::fixtures::resource_packet())?;
            let state = prepared.next.states.get_mut(&2).unwrap();
            state.fluid_kind = 1;
            state.fluid = 1;
            if glass {
                state.flags |= 512;
            }
            prepared.material_resources([32, 16], &[255; 32 * 16 * 4])?;
            for quad in &prepared.next.states[&2].leaves[0].quads {
                let material = prepared.next.definitions[quad.material.index() as usize];
                assert_eq!(material.control[0], if glass { 3 } else { 0 });
                assert_ne!(material.extinction[0], 1.333);
                assert_ne!(material.control[2] & Definition::CUTOUT, 0);
            }
            let water = prepared.next.fluid_materials[&(2, 0)];
            let material = prepared.next.definitions[water.index() as usize];
            assert_eq!(material.control[0], 1);
            assert_eq!(material.extinction[0], 1.333);
        }
        Ok(())
    }

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
    fn end_rod_dark_base_keeps_the_zero_power_fallback() -> Result<(), Error> {
        let mut catalog = crate::Catalog::default();
        let mut prepared = catalog.stage_resources(&crate::fixtures::resource_packet())?;
        let state = prepared.next.states.get_mut(&1).unwrap();
        state.emission = 14;
        let model = state.model;
        let sprite = prepared.next.sprites.get_mut(&1).unwrap();
        sprite.name = "minecraft:block/end_rod".into();
        sprite.vanilla = true;
        let bounds = sprite.bounds;
        let Model::Mesh(quads) = prepared.next.models.get_mut(&model).unwrap() else {
            unreachable!("夹具的固定立方体模型")
        };
        for quad in quads {
            quad.uv = [
                [0.375, 0.375],
                [0.375, 0.125],
                [0.125, 0.125],
                [0.125, 0.375],
            ]
            .map(|point| {
                std::array::from_fn(|axis| {
                    bounds[axis] + point[axis] * (bounds[axis + 2] - bounds[axis])
                })
            });
        }
        prepared.material_resources([32, 16], &[255; 32 * 16 * 4])?;
        for quad in &prepared.next.states[&1].leaves[0].quads {
            let material = prepared.next.definitions[quad.material.index() as usize];
            assert_ne!(material.control[2] & Definition::TEXTURED_EMISSION, 0);
            assert_eq!(material.emission[3], 1.);
        }
        Ok(())
    }

    #[test]
    fn grass_sides_bind_only_one_quad_with_the_overlay_tint() -> Result<(), Error> {
        check_grass_layers(false)
    }

    #[test]
    fn grass_with_lab_maps_keeps_the_biome_layer_on_each_side() -> Result<(), Error> {
        check_grass_layers(true)
    }

    fn check_grass_layers(lab: bool) -> Result<(), Error> {
        let mut catalog = crate::Catalog::default();
        let image = |rgba: [u8; 4]| ImageSource {
            width: 1,
            height: 1,
            pixels: rgba.to_vec(),
        };
        let maps = lab.then(|| {
            std::array::from_fn(|_| (image([133, 127, 250, 255]), image([86, 10, 0, 255])))
        });
        let mut prepared = crate::fixtures::grass_block(&mut catalog, maps)?;
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
            let maps = Definition::MERS | Definition::OVERLAY | Definition::OVERLAY_MERS;
            let lab_maps = Definition::AUXILIARY
                | Definition::LAB_SPECULAR
                | Definition::OVERLAY_AUXILIARY
                | Definition::OVERLAY_NORMAL
                | Definition::OVERLAY_LAB_SPECULAR;
            assert_eq!(
                material.control[2],
                if lab { maps | lab_maps } else { maps }
            );
            assert_eq!(
                material.control[3],
                if lab { Definition::AUTHORED_NORMAL } else { 0 }
            );
            assert!(material.valid());
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
    fn custom_emission_masks_other_block_faces_and_states() -> Result<(), Error> {
        let mut catalog = crate::Catalog::default();
        let mut prepared = catalog.stage_resources(&crate::fixtures::resource_packet())?;
        for (id, name) in [(1, "jack_o_lantern"), (2, "pumpkin_side")] {
            let sprite = prepared.next.sprites.get_mut(&id).unwrap();
            sprite.name = format!("minecraft:block/{name}");
            sprite.vanilla = true;
        }
        for id in [1, 2] {
            let state = prepared.next.states.get_mut(&id).unwrap();
            state.name = "minecraft:jack_o_lantern".into();
            state.emission = 15;
            let Model::Mesh(quads) = prepared.next.models.get_mut(&id).unwrap() else {
                unreachable!("固定六面模型");
            };
            for q in quads {
                if id == 2 || q.face != 2 {
                    q.sprite = 2;
                    for uv in &mut q.uv {
                        uv[0] += 0.5;
                    }
                }
            }
        }
        prepared.next.models.insert(
            3,
            Model::Weighted {
                children: vec![(1, 1)],
                total: 1,
            },
        );
        prepared.next.models.insert(4, Model::Multipart(vec![3]));
        prepared.next.states.get_mut(&1).unwrap().model = 4;
        // 独立发光方块复用南瓜侧面的模型和纹理，仍依赖原版亮度。
        let separate = prepared.next.states.get_mut(&3).unwrap();
        separate.model = 2;
        separate.fluid_kind = 0;
        separate.fluid = 0;
        separate.emission = 15;
        let Model::Mesh(quads) = prepared.next.models.get_mut(&2).unwrap() else {
            unreachable!()
        };
        for q in quads {
            q.tint = -1;
        }
        let maps = prepared.material_resources([32, 16], &[255; 32 * 16 * 4])?;
        assert_eq!(
            maps.item_materials.surface(Some(2), 2, 0, false),
            Some((0, 0.))
        );
        assert_eq!(
            maps.item_materials.surface(Some(3), 2, 0, false),
            Some((0, 1.))
        );
        for id in [1, 2, 3] {
            for q in &prepared.next.states[&id].leaves[0].quads {
                let m = prepared.next.definitions[q.material.index() as usize];
                assert_eq!(
                    m.emission[3] > 0.,
                    id == 3 || (id == 1 && q.index == 2),
                    "state={id}, quad={}",
                    q.index
                );
                assert_eq!(
                    m.control[2] & Definition::TEXTURED_EMISSION != 0,
                    id == 1 && q.index == 2
                );
                if m.emission[3] == 0. {
                    assert_eq!(m.control[3] >> 1, 0);
                }
            }
        }
        Ok(())
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
        assert_eq!(
            bilateral(&[SourceQuad { ..a }, reverse(true)], false),
            [Side::Front, Side::Front]
        );
        let b = reverse(false);
        assert_eq!(bilateral(&[a, b], false), [Side::Both, Side::Omit]);
    }

    #[test]
    fn floor_lichen_keeps_the_exposed_coverage_from_either_ray_direction() -> Result<(), Error> {
        for reverse_order in [false, true] {
            let mut catalog = crate::Catalog::default();
            let mut prepared = crate::fixtures::floor_lichen(&mut catalog, reverse_order)?;
            prepared.material_resources([32, 16], &[255; 32 * 16 * 4])?;
            let bound = &prepared.next.states[&1].leaves[0].quads;
            assert_eq!(bound.len(), 1);
            assert_eq!(bound[0].index, usize::from(!reverse_order));
            let bits = prepared.next.definitions[bound[0].material.index() as usize].control[2];
            assert_eq!(
                bits & (Definition::CUTOUT | Definition::FRONT_ONLY | Definition::TWO_SIDED),
                Definition::CUTOUT | Definition::TWO_SIDED
            );
        }
        Ok(())
    }
}
