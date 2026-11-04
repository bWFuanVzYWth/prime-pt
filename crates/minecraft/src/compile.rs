//! 同步、无隐藏调度的section/slab内核；邻域与资源只借用，输出独占。

use crate::TintSpan;
use crate::{
    Catalog, Error, SectionStorage,
    resources::{Model, State},
};
use prime_scene::{Mesh, Quad};

/// 一个四层slab的显式输入；调度者共享邻域、独占每个输出，不在内核创建线程池。
#[derive(Clone, Copy, Debug)]
pub struct SlabCompile {
    /// section内首Y，仅允许0/4/8/12。
    pub first_y: usize,
    /// section在64m region内的基准，每轴为0/16/32/48；输出直接使用最终region-local坐标。
    pub origin: [f32; 3],
    /// 本份输出额度；同时限制索引域为u32，超额在push之前返回OutputLimit。
    pub max_quads: usize,
}

/// 一个slab的独占输出；World保留分片，不为组装region复制quad。
#[derive(Default)]
pub struct CompiledSlab {
    /// 最终region-local真实quad。
    pub mesh: Mesh,
    /// 持久tint来源；同block/source连续quad共用span，应用颜色后仍保留。
    pub bindings: Vec<TintSpan>,
}
impl CompiledSlab {
    pub(crate) fn bind(&mut self, quad: usize, source: std::num::NonZeroU32, local: [i32; 3]) {
        let local = (local[0] | local[2] << 4 | local[1] << 8) as u16;
        if let Some(last) = self.bindings.last_mut()
            && last.source == source.get()
            && last.local == local
            && u64::from(last.first) + u64::from(last.count) == quad as u64
        {
            last.count += 1;
        } else {
            self.bindings.push(TintSpan {
                first: quad as u32,
                count: 1,
                source: source.get(),
                local,
                reserved: 0,
            });
        }
    }
    /// 按span顺序应用完整ARGB；先校验全部范围，失败不改变mesh或持久绑定。
    pub fn apply_tints(&mut self, colors: &[u32]) -> Result<(), Error> {
        if colors.len() != self.bindings.len() {
            return Err(Error::Input("slab tint响应数量不匹配"));
        }
        let mut end = 0u64;
        for binding in &self.bindings {
            binding.validate(self.mesh.quads.len())?;
            if u64::from(binding.first) < end {
                return Err(Error::Input("slab tint span重叠或顺序无效"));
            }
            end = u64::from(binding.first) + u64::from(binding.count);
        }
        for (binding, color) in self.bindings.iter().zip(colors) {
            let tint = [16, 8, 0].map(|shift| ((color >> shift) & 255) as f32 / 255.);
            for quad in &mut self.mesh.quads[binding.first as usize
                ..(u64::from(binding.first) + u64::from(binding.count)) as usize]
            {
                quad.set_tint(tint);
            }
        }
        Ok(())
    }
    #[cfg(any(test, feature = "fixtures"))]
    fn legacy(self, catalog: &Catalog, key: [i32; 3]) -> Result<CompiledSection, Error> {
        let mut tints = Vec::new();
        for binding in self.bindings {
            let request = catalog.tint_request(catalog.generation(), key, binding)?;
            tints.extend(
                (binding.first as usize
                    ..(u64::from(binding.first) + u64::from(binding.count)) as usize)
                    .map(|quad| TintPatch { quad, request }),
            );
        }
        Ok(CompiledSection {
            mesh: self.mesh,
            tints,
        })
    }
}

/// 一个邻域位置的真实可用状态；Unknown不能作为air参与编译。
#[derive(Clone, Copy)]
pub enum Neighbor<'a> {
    /// 已接受同资源世代的压缩section。
    Available(&'a SectionStorage),
    /// 世界高度域外等由调用者边界证明为空。
    Empty,
    /// 宿主明确未加载；调用者保留旧覆盖或延后，不猜测方块。
    Unavailable,
    /// 尚未捕获，必须延后。
    Unknown,
}

/// 中心section及完整一格section halo；实际只解包各邻居的一block边界。
#[derive(Clone, Copy)]
pub struct Neighborhood<'a> {
    /// 中心16m section坐标。
    pub key: [i32; 3],
    /// 已接受资源世代，必须与Catalog相同。
    pub generation: u64,
    /// ((dy+1)*3+(dz+1))*3+(dx+1)，中心索引13。
    pub sections: [Neighbor<'a>; 27],
}

/// 必要的世界相关tint源求值；编译器不调用Java，也不猜测第三方tint函数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TintRequest {
    /// 绝对方块位置。
    pub position: [i32; 3],
    /// 真实registry状态ID。
    pub state: u32,
    /// 方块乘色槽，-1表示实际流体tint源。
    pub slot: i32,
}

/// 一个quad所需的tint结果；world完成回调前不得发布其mesh。
#[cfg(any(test, feature = "fixtures"))]
pub struct TintPatch {
    /// 本输出Mesh内的quad索引。
    pub quad: usize,
    /// 真实宿主求值输入，可由上层按完整键去重。
    pub request: TintRequest,
}

/// 自持几何与显式后续求值；不持有Catalog或输入包引用。
#[cfg(any(test, feature = "fixtures"))]
#[derive(Default)]
pub struct CompiledSection {
    /// slab或section局部坐标的真实四角，保持源primitive次序。
    pub mesh: Mesh,
    /// 未完成宿主tint；空数组表示可立即发布。
    pub tints: Vec<TintPatch>,
}

#[cfg(any(test, feature = "fixtures"))]
impl CompiledSection {
    /// 按patch顺序应用真实ARGB结果；先验证完整长度，失败保留原mesh。
    pub fn apply_tints(&mut self, colors: &[u32]) -> Result<(), Error> {
        if colors.len() != self.tints.len() {
            return Err(Error::Input("tint响应数量不匹配"));
        }
        for (patch, color) in self.tints.iter().zip(colors) {
            self.mesh.quads[patch.quad]
                .set_tint([16, 8, 0].map(|shift| ((color >> shift) & 255) as f32 / 255.));
        }
        self.tints.clear();
        Ok(())
    }
}

impl Neighborhood<'_> {
    pub(crate) fn state(&self, position: [i32; 3]) -> Result<Option<u32>, Error> {
        let offset = position.map(|v| v.div_euclid(16));
        if offset.into_iter().any(|v| !(-1..=1).contains(&v)) {
            return Err(Error::Input("编译邻域越界"));
        }
        let index = ((offset[1] + 1) * 9 + (offset[2] + 1) * 3 + offset[0] + 1) as usize;
        match self.sections[index] {
            Neighbor::Available(storage) => Ok(storage.state(
                ((position[1].rem_euclid(16) * 16 + position[2].rem_euclid(16)) * 16
                    + position[0].rem_euclid(16)) as usize,
            )),
            Neighbor::Empty => Ok(None),
            Neighbor::Unavailable | Neighbor::Unknown => {
                Err(Error::MissingNeighbor(std::array::from_fn(|axis| {
                    self.key[axis] + offset[axis]
                })))
            }
        }
    }
}

const DIRECTIONS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

fn position_seed(position: [i32; 3]) -> i64 {
    let [x, y, z] = position;
    let value = i64::from(x.wrapping_mul(3_129_871))
        ^ i64::from(z).wrapping_mul(116_129_781)
        ^ i64::from(y);
    value
        .wrapping_mul(value)
        .wrapping_mul(42_317_861)
        .wrapping_add(value.wrapping_mul(11))
        >> 16
}

#[derive(Clone, Copy)]
struct Random(u64);
impl Random {
    fn new(seed: i64) -> Self {
        Self((seed as u64 ^ 0x5deece66d) & ((1u64 << 48) - 1))
    }
    fn bits(&mut self, bits: u32) -> u32 {
        self.0 = (self.0.wrapping_mul(0x5deece66d).wrapping_add(11)) & ((1u64 << 48) - 1);
        (self.0 >> (48 - bits)) as u32
    }
    fn long(&mut self) -> i64 {
        (i64::from(self.bits(32) as i32) << 32).wrapping_add(i64::from(self.bits(32) as i32))
    }
    fn bound(&mut self, bound: u32) -> u32 {
        if bound.is_power_of_two() {
            return ((u64::from(bound) * u64::from(self.bits(31))) >> 31) as u32;
        }
        loop {
            let bits = self.bits(31);
            let value = bits % bound;
            if (bits as i32)
                .wrapping_sub(value as i32)
                .wrapping_add(bound as i32 - 1)
                >= 0
            {
                return value;
            }
        }
    }
}

fn placement(state: &State, position: [i32; 3]) -> Result<([f32; 3], i64), Error> {
    let mut seed_position = position;
    match state.placement.seed {
        1 => seed_position[1] = seed_position[1].wrapping_sub(1),
        2 => seed_position[2] = seed_position[2].wrapping_sub(1),
        3 => seed_position[2] = seed_position[2].wrapping_add(1),
        4 => seed_position[0] = seed_position[0].wrapping_sub(1),
        5 => seed_position[0] = seed_position[0].wrapping_add(1),
        6 => return Err(Error::Input("自定义位置seed需要显式宿主结果")),
        _ => {}
    }
    let offset = match state.placement.kind {
        0 => [0.; 3],
        1 | 2 => {
            let value = position_seed([position[0], 0, position[2]]);
            let nibble = |shift: u32| f64::from(((value >> shift) & 15i64) as f32 / 15.);
            let h = f64::from(state.placement.horizontal);
            [
                ((nibble(0) - 0.5) * 0.5).clamp(-h, h) as f32,
                if state.placement.kind == 2 {
                    ((nibble(4) - 1.) * f64::from(state.placement.vertical)) as f32
                } else {
                    0.
                },
                ((nibble(8) - 0.5) * 0.5).clamp(-h, h) as f32,
            ]
        }
        _ => return Err(Error::Input("自定义位置offset需要显式宿主结果")),
    };
    Ok((offset, position_seed(seed_position)))
}

fn covered(catalog: &Catalog, source: u32, neighbor: u32) -> bool {
    if neighbor == 1 {
        return true;
    }
    if source == 0 || neighbor == 0 {
        return false;
    }
    let Some(other) = catalog.faces.get(&neighbor) else {
        return false;
    };
    let full = [0., 1.];
    let (us, vs, words) = if source == 1 {
        (&full[..], &full[..], &[1u64][..])
    } else {
        let Some(face) = catalog.faces.get(&source) else {
            return false;
        };
        (&face.u[..], &face.v[..], &face.words[..])
    };
    let bits = |words: &[u64], width: usize, u: usize, v: usize| {
        let bit = u * width + v;
        words[bit / 64] & (1u64 << (bit % 64)) != 0
    };
    for u in 0..us.len() - 1 {
        for v in 0..vs.len() - 1 {
            if !bits(words, vs.len() - 1, u, v) {
                continue;
            }
            if other.u[0] > us[u]
                || other.u[other.u.len() - 1] < us[u + 1]
                || other.v[0] > vs[v]
                || other.v[other.v.len() - 1] < vs[v + 1]
            {
                return false;
            }
            for a in 0..other.u.len() - 1 {
                for b in 0..other.v.len() - 1 {
                    if other.u[a + 1] > us[u]
                        && other.u[a] < us[u + 1]
                        && other.v[b + 1] > vs[v]
                        && other.v[b] < vs[v + 1]
                        && !bits(&other.words, other.v.len() - 1, a, b)
                    {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// 编译一个4层slab；first_y必须为0/4/8/12。无线程池、无嵌套调度。
/// 未知资源或必要邻域返回错误，输出不发布；world在全部slab/tint完成后合并发布。
#[cfg(any(test, feature = "fixtures"))]
pub fn compile_section_slab(
    catalog: &Catalog,
    neighborhood: &Neighborhood<'_>,
    first_y: usize,
) -> Result<CompiledSection, Error> {
    compile_section_slab_limited(catalog, neighborhood, first_y, usize::MAX)
}

/// 调度层给定最大输出quad数的slab入口；每个模型quad扩容前检查额度。
/// 流体每块至多六个面，在临时独占输出中同样受此额度限制。
#[cfg(any(test, feature = "fixtures"))]
pub fn compile_section_slab_limited(
    catalog: &Catalog,
    neighborhood: &Neighborhood<'_>,
    first_y: usize,
    max_quads: usize,
) -> Result<CompiledSection, Error> {
    compile_slab(
        catalog,
        neighborhood,
        SlabCompile {
            first_y,
            origin: [0.; 3],
            max_quads,
        },
    )?
    .legacy(catalog, neighborhood.key)
}

/// 编译一个已校验世代的slab，直接写region-local坐标并返回持久紧凑tint绑定。
/// 共享只读Neighborhood可被四个并行slab复用；缺source返回错误，不发布部分输出。
pub fn compile_slab(
    catalog: &Catalog,
    neighborhood: &Neighborhood<'_>,
    options: SlabCompile,
) -> Result<CompiledSlab, Error> {
    let SlabCompile {
        first_y,
        origin,
        max_quads,
    } = options;
    let max_quads = max_quads.min(u32::MAX as usize);
    if ![0, 4, 8, 12].contains(&first_y)
        || catalog.generation() != neighborhood.generation
        || origin.iter().any(|v| ![0., 16., 32., 48.].contains(v))
    {
        return Err(Error::Input("slab范围或资源世代无效"));
    }
    match neighborhood.sections[13] {
        Neighbor::Empty => return Ok(CompiledSlab::default()),
        Neighbor::Available(storage) => {
            if let Some(id) = storage.uniform_state() {
                let state = catalog
                    .states
                    .get(&id)
                    .ok_or(Error::Input("section引用未准备的状态ID"))?;
                if state.flags & 1 != 0 || state.flags & 16 != 0 && state.fluid_kind == 0 {
                    return Ok(CompiledSlab::default());
                }
            }
        }
        _ => {}
    }
    let mut output = CompiledSlab::default();
    let mut stack = Vec::new();
    for y in first_y..first_y + 4 {
        for z in 0..16 {
            for x in 0..16 {
                let local = [x, y as i32, z];
                let Some(id) = neighborhood.state(local)? else {
                    continue;
                };
                let state = catalog
                    .states
                    .get(&id)
                    .ok_or(Error::Input("section引用未准备的状态ID"))?;
                if state.flags & 1 != 0 {
                    continue;
                }
                let absolute =
                    std::array::from_fn(|axis| neighborhood.key[axis] * 16 + local[axis]);
                if state.fluid_kind != 0 {
                    crate::fluid::emit(
                        catalog,
                        neighborhood,
                        crate::fluid::FluidBlock {
                            state,
                            state_id: id,
                            local,
                            origin,
                        },
                        &mut output,
                        max_quads,
                    )?;
                }
                if state.flags & 16 != 0 || state.model == 0 {
                    continue;
                }
                let mut visible = state.face_mask;
                for (face, delta) in DIRECTIONS.iter().enumerate() {
                    if visible & (1 << face) == 0 {
                        continue;
                    }
                    let other = neighborhood
                        .state(std::array::from_fn(|axis| local[axis] + delta[axis]))?;
                    if let Some(other) = other {
                        let other = catalog
                            .states
                            .get(&other)
                            .ok_or(Error::Input("邻域引用未准备的状态ID"))?;
                        let same = state.matching_block != 0
                            && state.matching_block == other.matching_block;
                        if same || covered(catalog, state.faces[face], other.faces[face ^ 1]) {
                            visible &= !(1 << face);
                        }
                    }
                }
                if visible == 0 {
                    continue;
                }
                let (offset, seed) = placement(state, absolute)?;
                stack.clear();
                stack.push((state.model, Random::new(seed)));
                while let Some((model_id, mut random)) = stack.pop() {
                    let model = catalog
                        .models
                        .get(&model_id)
                        .ok_or(Error::Input("未准备的模型ID"))?;
                    match model {
                        Model::Unknown => return Err(Error::Input("未知模型不能伪装为空几何")),
                        Model::Alias(id) => stack.push((*id, random)),
                        Model::Multipart(children) => {
                            let seed = random.long();
                            for &child in children.iter().rev() {
                                stack.push((child, Random::new(seed)));
                            }
                        }
                        Model::Weighted { children, total } => {
                            let selected = random.bound(*total);
                            let index =
                                children.partition_point(|(cumulative, _)| *cumulative <= selected);
                            stack.push((children[index].1, random));
                        }
                        Model::Mesh(quads) => {
                            let leaf = state
                                .leaves
                                .binary_search_by_key(&model_id, |leaf| leaf.model)
                                .map_err(|_| Error::Input("模型材质尚未完成资源准备"))?;
                            for binding in &state.leaves[leaf].quads {
                                let quad = &quads[binding.index];
                                if visible & (1 << quad.face) == 0 {
                                    continue;
                                }
                                let material = binding.material;
                                let positions = quad.positions.map(|point| {
                                    std::array::from_fn(|axis| {
                                        point[axis]
                                            + offset[axis]
                                            + local[axis] as f32
                                            + origin[axis]
                                    })
                                });
                                let at = output.mesh.quads.len();
                                if at >= max_quads {
                                    return Err(Error::OutputLimit {
                                        required: at.saturating_add(1),
                                    });
                                }
                                output.mesh.quads.push(Quad::with_material(
                                    positions, quad.uv, [1.; 3], material,
                                ));
                                if let Some(source) = binding.tint {
                                    output.bind(at, source, local);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(output)
}

/// 串行完整section入口；调度层可直接并行调用四个slab而无需本wrapper。
#[cfg(any(test, feature = "fixtures"))]
pub fn compile_section(
    catalog: &Catalog,
    neighborhood: &Neighborhood<'_>,
) -> Result<CompiledSection, Error> {
    let mut output = CompiledSection::default();
    for first_y in [0, 4, 8, 12] {
        let mut part = compile_section_slab(catalog, neighborhood, first_y)?;
        let first = output.mesh.quads.len();
        for patch in &mut part.tints {
            patch.quad += first;
        }
        output.mesh.quads.extend(part.mesh.quads);
        output.tints.extend(part.tints);
    }
    Ok(output)
}

pub(crate) fn fluid_state<'a>(
    catalog: &'a Catalog,
    neighborhood: &Neighborhood<'_>,
    position: [i32; 3],
) -> Result<Option<&'a State>, Error> {
    neighborhood
        .state(position)?
        .map(|id| {
            catalog
                .states
                .get(&id)
                .ok_or(Error::Input("流体邻域引用未准备状态"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "先执行Gradle modelRandomFixture；实际26.3 LegacyRandomSource连续操作"]
    fn actual_java_random_stream_matches_model_selection_operations() {
        let path = std::env::var_os("PRIME_MODEL_RANDOM_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/java-model-random.bin")
            });
        let bytes = std::fs::read(path).unwrap();
        let mut reader = crate::wire::Reader::new(&bytes).unwrap();
        assert_eq!(reader.u32().unwrap(), 0x31474e52);
        assert_eq!(reader.u32().unwrap(), 263);
        for _ in 0..reader.u32().unwrap() {
            let seed = reader.u64().unwrap();
            let mut random = Random::new(seed as i64);
            for index in 0..reader.u32().unwrap() {
                let kind = reader.u32().unwrap();
                let bound = reader.u32().unwrap();
                let expected = reader.u64().unwrap();
                let actual = match kind {
                    0 => random.long() as u64,
                    1 => u64::from(random.bound(bound)),
                    _ => panic!("未知oracle操作"),
                };
                assert_eq!(
                    actual, expected,
                    "seed {seed:x}, operation {index}, kind {kind}, bound {bound}"
                );
            }
        }
        reader.finish().unwrap();
    }
    use crate::{
        decode_sections,
        fixtures::{self, Pattern},
    };

    #[test]
    fn dense_palette_uses_real_model_faces_and_culls_internal_boundaries() {
        let catalog = fixtures::catalog().unwrap();
        let sources =
            decode_sections(&fixtures::section_packet(&[(1, [0; 3], Pattern::Dense)])).unwrap();
        let mut sections = [Neighbor::Empty; 27];
        sections[13] = Neighbor::Available(sources[0].storage.as_ref().unwrap());
        let result = compile_section(
            &catalog,
            &Neighborhood {
                key: [0; 3],
                generation: 1,
                sections,
            },
        )
        .unwrap();
        assert_eq!(result.mesh.quads.len(), 6 * 16 * 16);
        assert!(result.tints.is_empty());
        assert!(result.mesh.quads.iter().all(|q| {
            (0..3).any(|axis| {
                q.positions().iter().all(|p| p[axis] == 0.)
                    || q.positions().iter().all(|p| p[axis] == 16.)
            })
        }));
    }

    #[test]
    fn source_air_declaration_skips_missing_halo_and_output_limit_stops_before_expansion() {
        let catalog = fixtures::catalog().unwrap();
        let sources = decode_sections(&fixtures::section_packet(&[
            (1, [0; 3], Pattern::Air),
            (2, [1, 0, 0], Pattern::Sparse),
        ]))
        .unwrap();
        let mut sections = [Neighbor::Unknown; 27];
        sections[13] = Neighbor::Available(sources[0].storage.as_ref().unwrap());
        assert!(
            compile_section(
                &catalog,
                &Neighborhood {
                    key: [0; 3],
                    generation: 1,
                    sections
                }
            )
            .unwrap()
            .mesh
            .quads
            .is_empty()
        );
        let mut sections = [Neighbor::Empty; 27];
        sections[13] = Neighbor::Available(sources[1].storage.as_ref().unwrap());
        assert!(matches!(
            compile_section_slab_limited(
                &catalog,
                &Neighborhood {
                    key: [1, 0, 0],
                    generation: 1,
                    sections
                },
                0,
                1
            ),
            Err(Error::OutputLimit { required: 2 })
        ));
    }

    #[test]
    fn tinted_models_remain_unpublished_until_the_actual_host_results_arrive() {
        let catalog = fixtures::catalog().unwrap();
        let sources = decode_sections(&fixtures::section_packet(&[(
            1,
            [3, 0, -2],
            Pattern::Tinted,
        )]))
        .unwrap();
        let mut sections = [Neighbor::Empty; 27];
        sections[13] = Neighbor::Available(sources[0].storage.as_ref().unwrap());
        let mut result = compile_section(
            &catalog,
            &Neighborhood {
                key: [3, 0, -2],
                generation: 1,
                sections,
            },
        )
        .unwrap();
        assert_eq!(result.mesh.quads.len(), 1536);
        assert_eq!(result.tints.len(), 1536);
        assert!(result.tints.iter().all(|p| p.request.state == 2
            && p.request.slot == 0
            && p.request.position[0] >= 48
            && p.request.position[2] < -16));
        assert!(result.apply_tints(&[]).is_err());
        assert_eq!(result.tints.len(), 1536);
        result.apply_tints(&vec![0xff12ab34; 1536]).unwrap();
        assert!(result.tints.is_empty());
        assert_eq!(
            result.mesh.quads[0].tint(),
            [0x12 as f32 / 255., 0xab as f32 / 255., 0x34 as f32 / 255.]
        );
    }
}
