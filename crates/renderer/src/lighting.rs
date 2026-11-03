//! 变化region的发光几何缓存；只有发光分布或属性槽改变才重建全局proposal。
use prime_scene::{MaterialDefinition, RenderView};
use scene_cache::{Cache, Prepared, Update};

use crate::gpu::Error;

#[derive(Clone, PartialEq)]
struct QuadLight {
    positions: [[f32; 3]; 4],
    local: u32,
    area: [u32; 4],
    sides: u32,
}

pub(super) struct RegionLights {
    key: [i32; 3],
    revision: u64,
    base: u32,
    lights: Vec<QuadLight>,
}

pub(super) struct Lighting {
    cache: Cache<RegionLights>,
    epoch: u64,
}

pub(super) struct Tables {
    pub selection: [u32; 4],
    pub meta: Vec<[u32; 4]>,
    pub vertices: Vec<[f32; 4]>,
    pub regions: Vec<[u32; 4]>,
    pub inverse: Vec<[u32; 4]>,
}

pub(super) struct Preparation {
    pub update: Update<RegionLights>,
    pub tables: Option<Tables>,
}

impl Lighting {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            cache: Cache::new()?,
            epoch: 0,
        })
    }

    pub fn prepare(
        &self,
        source: RenderView<'_>,
        bases: &[u32],
        side_masks: &[u32],
        areas: &[[u32; 4]],
        possible: bool,
        epoch: u64,
    ) -> Result<Preparation, Error> {
        let mut update = self.cache.begin(source.regions.len())?;
        let (_, entries, _) = update.parts_mut();
        let mut changed = false;
        for (region, &base) in source.regions.iter().zip(bases) {
            let old = self
                .cache
                .entries()
                .binary_search_by_key(&region.key, |r| r.key)
                .ok();
            if let Some(index) = old
                && self.epoch == epoch
                && self.cache.entries()[index].revision == region.content_revision
                && self.cache.entries()[index].base == base
            {
                entries.push(Prepared::Reused(index));
                continue;
            }
            let mut lights = Vec::new();
            if possible {
                for (local, quad) in region.mesh.quads().enumerate() {
                    let &sides = side_masks
                        .get(quad.material_id().index() as usize)
                        .ok_or(Error::Invalid("quad材质索引不属于当前资源世代"))?;
                    if sides == 0 {
                        continue;
                    }
                    let positions = quad.positions();
                    let area = *areas
                        .get(quad.material_id().index() as usize)
                        .ok_or(Error::Invalid("quad静态面积身份不属于当前资源世代"))?;
                    if area[0] == u32::MAX && degenerate(positions) {
                        continue;
                    }
                    lights.push(QuadLight {
                        positions,
                        local: u32::try_from(local)
                            .map_err(|_| Error::Invalid("发光quad索引溢出"))?,
                        area,
                        sides,
                    });
                }
            }
            changed |= if let Some(i) = old {
                let old = &self.cache.entries()[i];
                old.lights != lights || (!lights.is_empty() && old.base != base)
            } else {
                !lights.is_empty()
            };
            entries.push(Prepared::Built(RegionLights {
                key: region.key,
                revision: region.content_revision,
                base,
                lights,
            }));
        }
        changed |= self.cache.entries().iter().any(|old| {
            !old.lights.is_empty()
                && source
                    .regions
                    .binary_search_by_key(&old.key, |r| r.key)
                    .is_err()
        });
        let tables = changed
            .then(|| build_tables(entries, self.cache.entries()))
            .transpose()?;
        Ok(Preparation { update, tables })
    }

    pub fn publication(
        &mut self,
        update: Update<RegionLights>,
    ) -> Result<scene_cache::Publication<'_, RegionLights>, Error> {
        Ok(self.cache.prepare(update, |_| 0..0)?)
    }

    pub fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }
}

pub(super) fn emitting(material: &MaterialDefinition) -> bool {
    material.control[1] != 0
        && material.emission[3] > 0.0
        && (material.emission[..3].iter().any(|&v| v > 0.0)
            || material.control[2] & MaterialDefinition::TEXTURED_EMISSION != 0)
}

fn degenerate(p: [[f32; 3]; 4]) -> bool {
    [[0, 1, 2], [0, 2, 3]].into_iter().all(|[a, b, c]| {
        let x = std::array::from_fn::<_, 3, _>(|i| f64::from(p[b][i]) - f64::from(p[a][i]));
        let y = std::array::from_fn::<_, 3, _>(|i| f64::from(p[c][i]) - f64::from(p[a][i]));
        [
            x[1] * y[2] - x[2] * y[1],
            x[2] * y[0] - x[0] * y[2],
            x[0] * y[1] - x[1] * y[0],
        ]
        .into_iter()
        .all(|v| v == 0.0)
    })
}

fn build_tables(
    entries: &[Prepared<RegionLights>],
    current: &[RegionLights],
) -> Result<Tables, Error> {
    let count = entries
        .iter()
        .map(|r| r.get(current).lights.len())
        .sum::<usize>();
    let count_u32 = u32::try_from(count).map_err(|_| Error::Invalid("活动发光quad数量超过u32"))?;
    if count_u32 > light_sampling::SELECTORS {
        return Err(Error::Invalid("活动发光quad数量超过24-bit均匀采样支持域"));
    }
    let mut meta = Vec::with_capacity(count);
    let mut vertices = Vec::with_capacity(count * 3);
    let mut regions = Vec::with_capacity(count);
    let mut inverse = Vec::with_capacity(count);
    for entry in entries {
        let entry = entry.get(current);
        for light in &entry.lights {
            let slot = entry
                .base
                .checked_add(light.local)
                .ok_or(Error::Invalid("发光属性槽溢出"))?;
            let index = u32::try_from(meta.len()).map_err(|_| Error::Invalid("发光表索引溢出"))?;
            let mut area = light.area;
            area[3] = slot;
            meta.push(area);
            vertices.extend(light_sampling::pack_vertices(light.positions));
            regions.push([
                entry.key[0] as u32,
                entry.key[1] as u32,
                entry.key[2] as u32,
                light.sides,
            ]);
            inverse.push([
                slot,
                index,
                0,
                slot.checked_add(1)
                    .ok_or(Error::Invalid("发光反查身份溢出"))?,
            ]);
        }
    }
    // 世界实例只维护注册数量。texel alias属于资源世代，不能在区块更新中构建。
    Ok(Tables {
        selection: [count_u32, 0, 0, 0],
        meta,
        vertices,
        regions,
        inverse: scene_cache::directory(&inverse)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prime_scene::{MaterialId, Mesh, Quad, RenderRegion};

    #[test]
    fn nonemitting_region_changes_preserve_the_light_distribution() {
        let mut lighting = Lighting::new().unwrap();
        let mesh = Mesh {
            quads: vec![Quad::with_material(
                [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
                [[0., 0.]; 4],
                [1.; 3],
                MaterialId::new(0),
            )],
        };
        let materials = [0];
        for revision in [1, 2] {
            let regions = [RenderRegion {
                key: [0; 3],
                geometry_revision: revision,
                content_revision: revision,
                mesh: (&mesh).into(),
            }];
            let prep = lighting
                .prepare(
                    RenderView {
                        generation: 0,
                        regions: &regions,
                        anchor: [0.; 3],
                    },
                    &[0],
                    &materials,
                    &[[u32::MAX, 0, 0, 0]],
                    false,
                    1,
                )
                .unwrap();
            if revision == 2 {
                assert!(prep.tables.is_none());
            }
            lighting.publication(prep.update).unwrap().commit();
            lighting.set_epoch(1);
        }
    }
}
