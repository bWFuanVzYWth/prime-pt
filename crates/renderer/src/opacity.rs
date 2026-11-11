//! 世代内唯一的精确 OMM；只在 replace_resources 准备，region 更新只反查。
use super::{Error, MaterialDefinition, ResourceUpload};
use simple_engine::{
    opacity,
    vulkan::{self, native::Context},
};
use std::collections::HashMap;
pub(super) type Key = (u32, [[u32; 2]; 4]);
#[derive(Default)]
pub(super) struct Prepared {
    pub map: Option<vulkan::OpacityMicromap>,
    pub(super) indices: HashMap<Key, [u32; 2]>,
}
impl Prepared {
    pub fn new(
        context: &mut Context,
        source: &ResourceUpload<'_>,
        definitions: &[MaterialDefinition],
    ) -> Result<Self, Error> {
        let Some(limit) = context.max_opacity2_state_subdivision_level() else {
            return Ok(Self::default());
        };
        let mut indices = HashMap::new();
        let mut intern = HashMap::new();
        for quad in source.opacity_quads {
            let material = definitions
                .get(quad.material as usize)
                .ok_or(Error::Invalid("OMM 材质身份无效"))?;
            let key = (quad.material, quad.uv.map(|p| p.map(f32::to_bits)));
            if indices.contains_key(&key) {
                continue;
            }
            if material.control[2] & (MaterialDefinition::CUTOUT | MaterialDefinition::FRONT_ONLY)
                != MaterialDefinition::CUTOUT
            {
                continue;
            }
            let lo = std::array::from_fn::<_, 2, _>(|i| {
                (material.uv_bounds[i] * source.extent[i] as f32)
                    .floor()
                    .clamp(0., (source.extent[i] - 1) as f32) as u32
            });
            let hi = std::array::from_fn::<_, 2, _>(|i| {
                ((material.uv_bounds[i + 2] * source.extent[i] as f32).ceil() - 1.)
                    .clamp(lo[i] as f32, (source.extent[i] - 1) as f32) as u32
                    + 1
            });
            let texture = opacity::Texture {
                extent: source.extent,
                rgba: source.albedo,
                bounds: [lo[0], lo[1], hi[0], hi[1]],
                cutoff: 128,
            };
            let mut pair = [vulkan::OPACITY_UNKNOWN; 2];
            for (half, corners) in [[0, 1, 2], [0, 2, 3]].into_iter().enumerate() {
                if let Some(map) =
                    opacity::aligned(&texture, corners.map(|i| quad.uv[i]), limit.min(10) as u16)?
                {
                    // 相同内容只驻留一次；hash 键在准备结束释放，GPU 目录不复制位图。
                    let map_key = (map.level, map.data);
                    if let Some(&id) = intern.get(&map_key) {
                        pair[half] = id;
                    } else {
                        let id = u32::try_from(intern.len())
                            .map_err(|_| Error::Invalid("OMM 索引溢出"))?;
                        intern
                            .try_reserve(1)
                            .map_err(simple_engine::scene_cache::Error::from)?;
                        intern.insert(map_key, id);
                        pair[half] = id;
                    }
                }
            }
            if pair != [vulkan::OPACITY_UNKNOWN; 2] {
                indices
                    .try_reserve(1)
                    .map_err(simple_engine::scene_cache::Error::from)?;
                indices.insert(key, pair);
            }
        }
        let mut descriptors = Vec::new();
        descriptors
            .try_reserve_exact(intern.len())
            .map_err(simple_engine::scene_cache::Error::from)?;
        descriptors.resize(
            intern.len(),
            vulkan::OpacityMap {
                subdivision_level: 0,
                data: &[],
            },
        );
        for ((level, data), &id) in &intern {
            descriptors[id as usize] = vulkan::OpacityMap {
                subdivision_level: *level,
                data,
            };
        }
        let map = if descriptors.is_empty() {
            None
        } else {
            Some(context.opacity_micromap(&descriptors)?)
        };
        eprintln!(
            "[prime_pt] omm_prepare quads={} maps={} bytes={}",
            indices.len(),
            intern.len(),
            intern.keys().map(|(_, v)| v.len()).sum::<usize>()
        );
        Ok(Self { map, indices })
    }
    pub fn indices(&self, material: u32, uv: [[f32; 2]; 4]) -> [u32; 2] {
        self.indices
            .get(&(material, uv.map(|p| p.map(f32::to_bits))))
            .copied()
            .unwrap_or([vulkan::OPACITY_UNKNOWN; 2])
    }
}
