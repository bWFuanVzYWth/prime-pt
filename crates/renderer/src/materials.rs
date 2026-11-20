//! 材质世代只扫描已绑定的 sprite，按矩形与覆盖规则共享统计，再为每个操作选择白名单组合。
use crate::gpu::Error;
use prime_scene::MaterialDefinition;
use simple_engine::{openpbr, scene_cache};

// mod_materials.slang 的散射颜色输入位；金属 Fresnel 仍需执行。
const BLACK_CONDUCTOR: u32 = 1 << 13;
const OVERLAY_COVERED: u32 = 1 << 14;
const OVERLAY_EMPTY: u32 = 1 << 15;

/// 只有这些覆盖规则会拒绝硬件三角候选；透明度和光学透射彼此独立。
pub(super) const fn tests_coverage(definition: &MaterialDefinition) -> bool {
    coverage_flags(definition) != 0
}

const fn coverage_flags(definition: &MaterialDefinition) -> u32 {
    definition.control[2] & (MaterialDefinition::CUTOUT | MaterialDefinition::FRONT_ONLY)
}

/// 资源世代内命中阶段可能用到的材质能力。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct HitFeatures {
    pub coverage: u32,
    pub authored_normals: bool,
}

pub(super) fn hit_features(definitions: &[MaterialDefinition]) -> HitFeatures {
    let mut features = HitFeatures {
        coverage: 0,
        authored_normals: false,
    };
    for definition in definitions {
        features.coverage |= coverage_flags(definition);
        features.authored_normals |=
            definition.control[3] & MaterialDefinition::AUTHORED_NORMAL != 0;
    }
    features
}

pub(super) const fn emission_sides(material: &MaterialDefinition) -> u32 {
    if material.control[2] & MaterialDefinition::FRONT_ONLY != 0 {
        material.control[1] & 1
    } else {
        material.control[1]
    }
}

/// 已发布候选灯的能力上界，与资源发布时的Flow/side过滤相同。
/// 输入等长由prepare_material_generation建立；不把接收材质的MERS误当作发光纹理。
pub(super) const fn emitter_capabilities(definitions: &[MaterialDefinition], keys: &[Key]) -> u32 {
    let mut capabilities = 0;
    let mut index = 0;
    while index < definitions.len() {
        let definition = &definitions[index];
        if keys[index].0.emits() && emission_sides(definition) != 0 {
            if definition.control[3] >> 1 != 0 {
                capabilities |= 1;
            }
            if definition.control[2] & MaterialDefinition::CUTOUT != 0 {
                capabilities |= 2;
            }
            if definition.control[2] & MaterialDefinition::TEXTURED_EMISSION != 0 {
                capabilities |= 4;
            }
        }
        index += 1;
    }
    capabilities
}

/// 资源期的静态亮度提案；保留动态tint和MERS/动画实际求值的宿主适配边界。
/// 与Game共用线性亮度×幅度公式，source RGB由本适配器解释，不扫描运行期几何。
pub(super) fn emission_weights(definitions: &[MaterialDefinition]) -> Result<Vec<f32>, Error> {
    let mut weights = Vec::new();
    weights
        .try_reserve_exact(definitions.len())
        .map_err(scene_cache::Error::from)?;
    for definition in definitions {
        let [r, g, b, amplitude] = definition.emission;
        weights.push(simple_engine::light_sampling::emission_weight(
            [r, g, b],
            amplitude,
        )?);
    }
    Ok(weights)
}

/// `mod_materials.slang` 的场景材质能力位。只影响分支，相同路径仍共享数值参数表。
pub(super) const fn variant(flow: openpbr::FlowKey, material: &MaterialDefinition) -> u32 {
    let flags = material.control[2];
    let mut maps = flags & (MaterialDefinition::MERS | MaterialDefinition::AUXILIARY);
    let normal = if (maps & MaterialDefinition::AUXILIARY) != 0 {
        (material.control[3] & MaterialDefinition::AUTHORED_NORMAL) << 3
    } else {
        0
    };
    let lab_specular = flags & MaterialDefinition::LAB_SPECULAR != 0;
    if normal == 0
        && !lab_specular
        && flow.bits() & openpbr::FlowKey::CONDUCTOR_REFLECTION != 0
        && flow.bits() & openpbr::FlowKey::MIXED_METALNESS == 0
    {
        // 纯金属无介质 IOR；没有作者法线时也无 XY 和 NDF 消费者。
        maps &= !MaterialDefinition::AUXILIARY;
    }
    let emission = if flow.bits() & openpbr::FlowKey::EMISSION != 0 {
        let sides = emission_sides(material);
        if sides == 0 {
            0
        } else {
            (sides << 4) | (flags & MaterialDefinition::TEXTURED_EMISSION)
        }
    } else {
        0
    };
    let thin_sss = lab_specular
        && flags & (MaterialDefinition::TWO_SIDED | MaterialDefinition::FRONT_ONLY) != 0;
    maps | normal
        | ((lab_specular as u32) << 16)
        | ((thin_sss as u32) << 17)
        | emission
        | if flow.emits() && material.control[3] >> 1 != 0 {
            1 << 10
        } else {
            0
        }
        | ((material.control[0] & 3) << 6)
        | ((flags & (MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS)) << 1)
}

/// 重放/求值不消费发光，不能让发光面、朝向和贴图位扩大它们的程序键。
pub(super) const fn scattering_key(
    (flow, variant): (openpbr::FlowKey, u32),
) -> (openpbr::FlowKey, u32) {
    (
        flow.without_emission(),
        variant & !(4 | 16 | 32 | (1 << 10)),
    )
}

pub(super) fn defaults() -> Vec<MaterialDefinition> {
    let rough = MaterialDefinition::default();
    let mut smooth = rough;
    smooth.base_roughness[3] = 0.25;
    vec![rough, smooth]
}

/// 一张最近点 LOD0 图集中的半开像素矩形，与 shader 的 sprite_address 一致。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rect([u32; 4]);

impl Rect {
    fn new(b: [f32; 4], [w, h]: [u32; 2]) -> Self {
        let axis = |lo: f32, hi: f32, n: u32| {
            let first = ((lo * n as f32).floor() as u32).min(n - 1);
            let end = ((hi * n as f32).ceil() as u32).clamp(first + 1, n);
            (first, end)
        };
        let (x0, x1) = axis(b[0], b[2], w);
        let (y0, y1) = axis(b[1], b[3], h);
        Self([x0, y0, x1, y1])
    }
}

#[derive(Clone, Copy)]
struct Tile {
    metal: openpbr::Weight,
    sss: openpbr::Weight,
    dielectric_sss: Option<openpbr::Weight>,
    rough: openpbr::Range,
    ndf: openpbr::Range,
    fully_covered: bool,
    color_channels: u8,
    emitting_channels: u8,
}

fn weight(channel: u8) -> openpbr::Weight {
    match channel {
        0 => openpbr::Weight::Zero,
        255 => openpbr::Weight::One,
        _ => openpbr::Weight::Blend,
    }
}

/// 一个世代共享 sprite 统计，缓存随准备结束释放。覆盖层只扫描通过 alpha 的 texel。
struct Atlas<'a> {
    extent: [u32; 2],
    albedo: &'a [u8],
    mers: &'a [u8],
    aux: &'a [u8],
    cache: std::collections::BTreeMap<(Rect, bool), Option<Tile>>,
}

impl Atlas<'_> {
    fn tile(&mut self, bounds: [f32; 4], cutout: bool) -> Option<Tile> {
        let rect = Rect::new(bounds, self.extent);
        *self.cache.entry((rect, cutout)).or_insert_with(|| {
            let [x0, y0, x1, y1] = rect.0;
            let mut weights = None;
            let mut lower = [255; 2];
            let mut upper = [0; 2];
            let mut dielectric_sss: Option<openpbr::Weight> = None;
            let mut color_channels = 0;
            let mut emitting_channels = 0;
            let mut fully_covered = true;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * self.extent[0] + x) * 4) as usize;
                    let color = &self.albedo[i..i + 4];
                    if cutout && color[3] < 128 {
                        fully_covered = false;
                        continue;
                    }
                    let m = self.mers.get(i..i + 4).unwrap_or(&[0, 0, 255, 0]);
                    let n = self.aux.get(i + 2).copied().unwrap_or(0);
                    let (metal, sss) = (weight(m[0]), weight(m[3]));
                    weights = Some(weights.map_or(
                        (metal, sss),
                        |(a, b): (openpbr::Weight, openpbr::Weight)| (a.union(metal), b.union(sss)),
                    ));
                    if metal != openpbr::Weight::One {
                        dielectric_sss = Some(dielectric_sss.map_or(sss, |w| w.union(sss)));
                    }
                    let rgb = u8::from(color[0] != 0)
                        | u8::from(color[1] != 0) << 1
                        | u8::from(color[2] != 0) << 2;
                    color_channels |= rgb;
                    if m[1] != 0 {
                        emitting_channels |= rgb;
                    }
                    for (j, v) in [m[2], n].into_iter().enumerate() {
                        lower[j] = lower[j].min(v);
                        upper[j] = upper[j].max(v);
                    }
                }
            }
            weights.map(|(metal, sss)| {
                let range = |i: usize| {
                    openpbr::Range::new(f32::from(lower[i]) / 255., f32::from(upper[i]) / 255.)
                        .expect("有样本的字节范围")
                };
                Tile {
                    metal,
                    sss,
                    dielectric_sss,
                    rough: range(0),
                    ndf: range(1),
                    fully_covered,
                    color_channels,
                    emitting_channels,
                }
            })
        })
    }
}

pub(super) type Key = (openpbr::FlowKey, u32);

/// 越界组合保留源编号和覆盖，完整参数表、SBT、OMM及发光表共同使用有效定义。
pub(super) struct Bindings<'a> {
    pub definitions: std::borrow::Cow<'a, [MaterialDefinition]>,
    pub keys: Vec<Key>,
}

pub(super) fn bindings<'a>(
    generation: u64,
    definitions: &'a [MaterialDefinition],
    extent: [u32; 2],
    albedo: &[u8],
    mers: &[u8],
    aux: &[u8],
) -> Result<Bindings<'a>, Error> {
    use MaterialDefinition as M;
    let mut atlas = Atlas {
        extent,
        albedo,
        mers,
        aux,
        cache: Default::default(),
    };
    let mut effective = std::borrow::Cow::Borrowed(definitions);
    let mut keys = Vec::new();
    keys.try_reserve_exact(definitions.len())
        .map_err(scene_cache::Error::from)?;
    for (id, source) in definitions.iter().enumerate() {
        let key = match bind(&mut atlas, source) {
            Ok(key) => key,
            Err(reason) => {
                eprintln!(
                    "[prime_pt] WARN 材质世代 {generation}，材质 {id} 回退普通非金属：{reason}"
                );
                let mut default = M::default();
                default.control[2] = source.control[2] & (M::CUTOUT | M::FRONT_ONLY | M::TWO_SIDED);
                if source
                    .uv_bounds
                    .into_iter()
                    .all(|v| (0. ..=1.).contains(&v))
                    && source.uv_bounds[0] < source.uv_bounds[2]
                    && source.uv_bounds[1] < source.uv_bounds[3]
                {
                    default.uv_bounds = source.uv_bounds;
                }
                effective.to_mut()[id] = default;
                bind(&mut atlas, &default).map_err(Error::Invalid)?
            }
        };
        keys.push(key);
    }
    Ok(Bindings {
        definitions: effective,
        keys,
    })
}

fn bind(atlas: &mut Atlas<'_>, m: &MaterialDefinition) -> Result<Key, &'static str> {
    use MaterialDefinition as M;
    use openpbr::{Base, Range, Recipe, Weight};
    if !m.valid() || !m.uv_bounds.into_iter().all(|v| (0. ..=1.).contains(&v)) {
        return Err("参数、图层或 UV 超出声明域");
    }
    let flags = m.control[2];
    let overlay = flags & M::OVERLAY != 0;
    let top = if overlay {
        atlas.tile(m.overlay_bounds, true)
    } else {
        None
    };
    // 覆盖层只替换散射/MERS；作者法线和命中发光仍消费源底图。
    let source = atlas.tile(m.uv_bounds, flags & M::CUTOUT != 0);
    let base = if top.is_some_and(|t| t.fully_covered) {
        None
    } else {
        source
    };
    let scalar = || -> Result<_, &'static str> {
        Ok((
            Weight::new(m.surface[1]).map_err(|_| "金属度越界")?,
            Weight::new(m.surface[2]).map_err(|_| "SSS 越界")?,
            Range::point(m.base_roughness[3]).map_err(|_| "粗糙度越界")?,
        ))
    };
    let layer = |t: Option<Tile>, mapped: bool| -> Result<_, &'static str> {
        if mapped && let Some(t) = t {
            Ok((t.metal, t.sss, t.dielectric_sss, t.rough))
        } else {
            let (metal, sss, rough) = scalar()?;
            Ok((metal, sss, (metal != Weight::One).then_some(sss), rough))
        }
    };
    let (metal, mut sss, dielectric_sss, mut rough) = match (base, top) {
        (Some(_), Some(_)) => {
            let (m, s, d, r) = layer(base, flags & M::MERS != 0)?;
            let (tm, ts, td, tr) = layer(top, flags & M::OVERLAY_MERS != 0)?;
            (
                m.union(tm),
                s.union(ts),
                match (d, td) {
                    (Some(a), Some(b)) => Some(a.union(b)),
                    _ => d.or(td),
                },
                r.union(tr),
            )
        }
        (None, Some(_)) => layer(top, flags & M::OVERLAY_MERS != 0)?,
        _ => layer(base, flags & M::MERS != 0)?,
    };
    if m.control[0] == 0 && metal == Weight::Switch && sss != Weight::Blend {
        // 纯金属 texel 的二值 SSS 不参与非金属底层；任何内点都保留完整混合域。
        sss = dielectric_sss.unwrap_or(Weight::Zero);
    }
    if flags & M::LAB_SPECULAR != 0 && flags & (M::TWO_SIDED | M::FRONT_ONLY) == 0 {
        sss = Weight::Zero;
    }
    if flags & M::AUXILIARY != 0
        && m.control[3] & M::AUTHORED_NORMAL != 0
        && let Some(t) = source
    {
        rough = rough
            .with_normal_distribution(t.ndf)
            .map_err(|_| "法线分布越界")?;
    }
    let channels = |rgb: &[f32]| {
        u8::from(rgb[0] > 0.) | u8::from(rgb[1] > 0.) << 1 | u8::from(rgb[2] > 0.) << 2
    };
    let color_channels = base.map_or(0, |t| t.color_channels) | top.map_or(0, |t| t.color_channels);
    let black = channels(&m.base_roughness) & color_channels == 0;
    let recipe = match (m.control[0], metal, sss) {
        (0, m, s) => Recipe::opaque(m, if black { Base::Black } else { Base::Colored(s) }),
        // OpenPBR 的介电拓扑不消费 opaque metal/SSS 权重；共享 MERS 的这些通道
        // 不能新增闭包或触发默认材质回退，粗糙度和发光仍按其实际消费者保留。
        (1, _, _) => Recipe::Solid,
        (3, _, _) => Recipe::Thin,
        _ => return Err("白名单没有此表面拓扑"),
    };
    let tint = channels(&m.emission);
    let emits = emission_sides(m) != 0
        && m.emission[3] > 0.
        && source.is_some_and(|t| {
            let channels = if flags & M::TEXTURED_EMISSION != 0 {
                t.emitting_channels
            } else {
                t.color_channels
            };
            channels & tint != 0
        });
    let flow = recipe
        .flow(rough, openpbr::InterfaceIor::UNKNOWN, emits)
        .map_err(|_| "散射测度越界")?;
    // 黑色金属仍有掠射反射，颜色输入已知为零，省去散射路径的底色读取。
    let color = if black && flow.bits() & openpbr::FlowKey::CONDUCTOR_REFLECTION != 0 {
        BLACK_CONDUCTOR
    } else {
        0
    };
    let shape = if !overlay {
        0
    } else if top.is_none() {
        OVERLAY_EMPTY
    } else if top.is_some_and(|t| t.fully_covered) {
        OVERLAY_COVERED
    } else {
        0
    };
    Ok((
        flow,
        variant(flow, m) | recipe.weight_bits() | color | shape,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(m: MaterialDefinition, mers: [u8; 4], aux: [u8; 4]) -> Key {
        bindings(1, &[m], [1, 1], &[255; 4], &mers, &aux)
            .unwrap()
            .keys[0]
    }

    #[test]
    fn dielectric_maps_do_not_create_opaque_lobes() {
        for topology in [1, 3] {
            let mut material = MaterialDefinition::default();
            material.control[0] = topology;
            material.control[2] = MaterialDefinition::MERS;
            material.base_roughness[3] = 0.;
            let definitions = [material];
            let bound = bindings(
                1,
                &definitions,
                [1, 1],
                &[255; 4],
                &[89, 0, 64, 127],
                &[0; 4],
            )
            .unwrap();
            assert_eq!(bound.definitions.as_ref(), &definitions);
            let bits = bound.keys[0].0.bits();
            assert_ne!(bits & openpbr::FlowKey::CONTINUOUS_TRANSMISSION, 0);
            assert_eq!(
                bits & (openpbr::FlowKey::CONDUCTOR_REFLECTION | openpbr::FlowKey::SUBSURFACE),
                0
            );
        }
    }

    #[test]
    fn normal_distribution_controls_delta_support() {
        let mut m = MaterialDefinition::default();
        m.base_roughness[3] = 0.;
        m.control[3] = MaterialDefinition::AUTHORED_NORMAL;
        m.control[2] |= MaterialDefinition::AUXILIARY;
        assert_eq!(
            key(m, [0; 4], [128, 128, 0, 85]).0.bits() & openpbr::FlowKey::CONTINUOUS_REFLECTION,
            0
        );
        assert_ne!(
            key(m, [0; 4], [128, 128, 128, 85]).0.bits() & openpbr::FlowKey::CONTINUOUS_REFLECTION,
            0
        );
    }

    #[test]
    fn overlay_coverage_keeps_only_reachable_scattering() {
        let mut material = MaterialDefinition::default();
        material.control[2] = MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS;
        material.overlay_bounds = [0.5, 0., 1., 1.];
        material.uv_bounds = [0., 0., 0.5, 1.];
        assert!(material.valid());
        let defs = [material];
        let maps = [0, 0, 255, 0, 255, 0, 128, 0];
        let covered = bindings(1, &defs, [2, 1], &[255; 8], &maps, &[0; 8]).unwrap();
        let empty = bindings(
            1,
            &defs,
            [2, 1],
            &[255, 255, 255, 255, 255, 255, 255, 0],
            &maps,
            &[0; 8],
        )
        .unwrap();
        assert_ne!(
            covered.keys[0].0.bits() & openpbr::FlowKey::CONDUCTOR_REFLECTION,
            0
        );
        assert_eq!(covered.keys[0].0.bits() & openpbr::FlowKey::DIFFUSE, 0);
        assert_eq!(
            empty.keys[0].0.bits() & openpbr::FlowKey::CONDUCTOR_REFLECTION,
            0
        );
        assert_ne!(empty.keys[0].0.bits() & openpbr::FlowKey::DIFFUSE, 0);
    }

    #[test]
    fn sprite_weights_distinguish_binary_and_arbitrary_mixtures() {
        let mut m = MaterialDefinition::default();
        m.control[2] = MaterialDefinition::MERS;
        let mut left = m;
        left.uv_bounds[2] = 0.5;
        let mut right = m;
        right.uv_bounds[0] = 0.5;
        let defs = [left, right, m];
        let maps = [
            0, 0, 200, 0, 255, 0, 200, 255, 71, 0, 200, 41, 28, 0, 200, 109,
        ];
        let values = bindings(1, &defs, [4, 1], &[255; 16], &maps, &[0; 16]).unwrap();
        assert_eq!(values.keys[0].1 >> 11, 1);
        assert_eq!(values.keys[0].0.bits() & openpbr::FlowKey::SUBSURFACE, 0);
        assert_eq!(values.keys[1].1 >> 11, 0);
        assert_eq!(values.keys[1].0, values.keys[2].0);
        assert_ne!(
            values.keys[1].0.bits() & openpbr::FlowKey::MIXED_METALNESS,
            0
        );
    }

    #[test]
    fn unsupported_material_publishes_a_nonemitting_default_at_the_same_index() {
        let mut m = MaterialDefinition::default();
        m.control[0] = 2;
        m.emission = [1.; 4];
        let defs = [MaterialDefinition::default(), m];
        let values = bindings(1, &defs, [1, 1], &[255; 4], &[0; 4], &[0; 4]).unwrap();
        assert_eq!(values.definitions[1], defs[0]);
        assert_eq!(values.keys[0], values.keys[1]);
        assert!(!values.keys[1].0.emits());
    }

    #[test]
    fn black_sprites_keep_reflection_without_loading_a_diffuse_base() {
        use openpbr::FlowKey as F;
        let mut dielectric = MaterialDefinition::default();
        dielectric.base_roughness[..3].copy_from_slice(&[0., 1., 0.]);
        dielectric.surface[2] = 0.27;
        let mut metal = dielectric;
        metal.surface[1] = 1.;
        let mats = [dielectric, metal];
        let keys = bindings(1, &mats, [1, 1], &[255, 0, 0, 255], &[0; 4], &[0; 4])
            .unwrap()
            .keys;
        assert_eq!(
            keys[0].0.bits(),
            F::CONTINUOUS_REFLECTION | F::DIELECTRIC_REFLECTION
        );
        assert_eq!(
            keys[1].0.bits(),
            F::CONTINUOUS_REFLECTION | F::CONDUCTOR_REFLECTION
        );
        assert_ne!(keys[1].1 & BLACK_CONDUCTOR, 0);
    }

    #[test]
    fn scattering_program_ignores_emission_but_keeps_material_maps() {
        let flow = simple_engine::openpbr::Material::opaque([0.5; 3], 0.4, 1.5, 0., 0.)
            .unwrap()
            .flow(true);
        let (key, variant) = scattering_key((flow, 1 | 2 | 4 | 8 | 16 | 32));
        assert!(!key.emits());
        assert_eq!(variant, 1 | 2 | 8);
    }

    #[test]
    fn selected_emitter_bounds_follow_the_published_light_domain() {
        let plain = MaterialDefinition {
            emission: [1.; 4],
            control: [0, 1, 0, 0],
            ..Default::default()
        };
        let mut mapped = plain;
        mapped.control[2] = MaterialDefinition::MERS
            | MaterialDefinition::TEXTURED_EMISSION
            | MaterialDefinition::CUTOUT;
        mapped.control[3] = 2;
        let definitions = [plain, mapped];
        for (emission, expected) in [(0, 0), (255, 7)] {
            let prepared = bindings(
                1,
                &definitions,
                [1, 1],
                &[255; 4],
                &[0, emission, 255, 0],
                &[0; 4],
            )
            .unwrap();
            assert!(prepared.keys[0].0.emits());
            assert_eq!(prepared.keys[1].0.emits(), emission != 0);
            assert_eq!(emitter_capabilities(&definitions, &prepared.keys), expected);
        }
    }
}
