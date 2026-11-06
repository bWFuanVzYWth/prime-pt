//! ReSTIR稳定slab目录与交点恢复数据；源键/源版本由场景所有者提供。
//! 单一buffer为16B头、每quad64B原位置/region/token、容量尾部的活动身份目录。
//! 未改slab沿用token；当前primitive范围可搬移，历史通过目录恢复新范围。
use super::{Error, Gpu, RegionGpu};
use simple_engine::{scene_cache, vulkan::native::Buffer};
use std::ops::Range;

type Key = ([i32; 3], usize);
pub(super) struct GeometryUpdate {
    pub buffer: Option<Buffer>,
    pub capacity: u32,
    pub identity: Option<scene_cache::GeometryUpdate<Key>>,
    header: Option<[u32; 4]>,
    pub directory_offset: usize,
    data: Vec<[u32; 16]>,
    ranges: Vec<(usize, Range<usize>)>,
    retired: Vec<(usize, u32)>,
}
impl GeometryUpdate {
    pub fn writes(&self) -> Vec<(usize, &[u8])> {
        let mut writes: Vec<_> = self
            .ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&self.data[range.clone()])))
            .chain(
                self.retired
                    .iter()
                    .map(|(offset, value)| (*offset, bytemuck::bytes_of(value))),
            )
            .collect();
        if let Some(header) = &self.header {
            writes.push((0, bytemuck::bytes_of(header)));
        }
        if let Some(identity) = &self.identity {
            writes.push((
                self.directory_offset,
                bytemuck::cast_slice(identity.words()),
            ));
        }
        writes
    }
    pub fn written_bytes(&self) -> u64 {
        self.data.len() as u64 * 64
            + self.retired.len() as u64 * 4
            + self.header.map_or(0, |_| 16)
            + self
                .identity
                .as_ref()
                .map_or(0, |identity| identity.words().len() as u64 * 16)
    }
}
impl Gpu {
    pub(super) fn prepare_restir_geometry(
        &mut self,
        source: prime_scene::RenderView<'_>,
        entries: &[scene_cache::Prepared<RegionGpu>],
        end: u32,
    ) -> Result<GeometryUpdate, Error> {
        let mut prepared = GeometryUpdate {
            buffer: self.restir_geometry,
            capacity: self.restir_geometry_capacity,
            identity: None,
            header: None,
            directory_offset: 0,
            data: Vec::new(),
            ranges: Vec::new(),
            retired: Vec::new(),
        };
        if self.renderer_kind != prime_scene::RealtimeRenderer::RestirPt {
            prepared.buffer = None;
            prepared.capacity = 0;
            return Ok(prepared);
        }
        let first = self.restir_geometry.is_none();
        if !first
            && entries.len() == self.region_cache.entries().len()
            && entries
                .iter()
                .enumerate()
                .all(|(index, entry)| matches!(entry, scene_cache::Prepared::Reused(old) if *old == index))
            && self.geometry_identities.records().is_empty() == source.regions.is_empty()
        {
            return Ok(prepared);
        }
        let required = end
            .max(1)
            .checked_next_power_of_two()
            .ok_or(Error::Invalid("ReSTIR几何容量溢出"))?;
        let capacity = u64::from(required.max(if first {
            0
        } else {
            self.restir_geometry_capacity
        }));
        let directory_offset = 16 + capacity * 64;
        let mut sources = Vec::new();
        for (region, entry) in source.regions.iter().zip(entries) {
            let mut base = entry.get(self.region_cache.entries()).range.start;
            for (slot, mesh) in region.mesh.fragments() {
                sources.try_reserve(1).map_err(scene_cache::Error::from)?;
                let revision = region.mesh.fragment_revision(slot).unwrap_or(0);
                sources.push(scene_cache::GeometrySource {
                    // 连续兼容视图的region版本与稀疏片段版本属于不同来源域。
                    key: (region.key, (slot << 1) | usize::from(revision != 0)),
                    revision: if revision == 0 {
                        region.geometry_revision
                    } else {
                        revision
                    },
                    location: [
                        base,
                        u32::try_from(mesh.quads.len())
                            .map_err(|_| Error::Invalid("slab面数量溢出"))?,
                        0,
                    ],
                });
                base = base
                    .checked_add(mesh.quads.len() as u32)
                    .ok_or(Error::Invalid("slab属性偏移溢出"))?;
            }
        }
        let identity = self.geometry_identities.begin(&sources)?;
        let directory_capacity = identity
            .words()
            .len()
            .checked_next_power_of_two()
            .ok_or(Error::Invalid("ReSTIR活动域目录容量溢出"))?
            as u64;
        let bytes = directory_offset + directory_capacity * 16;
        let buffer = if let Some(old) = self.restir_geometry
            && old.len() >= bytes
        {
            old
        } else {
            let next = Buffer::mapped_device_local(&mut self.context, bytes)?;
            if let Some(old) = self.restir_geometry {
                old.copy_prefix_to(&mut self.context, &next, old.len())?;
            }
            next
        };
        let count = source
            .regions
            .iter()
            .zip(entries)
            .filter(|(_, entry)| first || matches!(entry, scene_cache::Prepared::Built(_)))
            .try_fold(0usize, |count, (region, _)| {
                count
                    .checked_add(region.mesh.len())
                    .ok_or(Error::Invalid("ReSTIR几何数量溢出"))
            })?;
        prepared
            .data
            .try_reserve_exact(count)
            .map_err(scene_cache::Error::from)?;
        let mut domain = 0;
        for (region, entry) in source.regions.iter().zip(entries) {
            let dirty = first || matches!(entry, scene_cache::Prepared::Built(_));
            let start = prepared.data.len();
            for (_, mesh) in region.mesh.fragments() {
                let token = identity.records()[domain].token;
                domain += 1;
                if dirty {
                    for quad in &mesh.quads {
                        let mut row = [0; 16];
                        for (index, value) in quad.positions().into_iter().flatten().enumerate() {
                            row[index] = value.to_bits();
                        }
                        row[12..15].copy_from_slice(&region.key.map(|value| value as u32));
                        row[15] = token;
                        prepared.data.push(row);
                    }
                }
            }
            if dirty {
                prepared.ranges.push((
                    16 + entry.get(self.region_cache.entries()).range.start as usize * 64,
                    start..prepared.data.len(),
                ));
            }
        }
        if !first {
            let mut current = 0;
            for old in self.region_cache.entries() {
                while current < source.regions.len() && source.regions[current].key < old.key {
                    current += 1;
                }
                if current < source.regions.len()
                    && entries[current].get(self.region_cache.entries()).identity == old.identity
                {
                    continue;
                }
                prepared.retired.extend(
                    old.range
                        .clone()
                        .map(|slot| (16 + slot as usize * 64 + 60, 0)),
                );
            }
        }
        prepared.buffer = Some(buffer);
        prepared.capacity = capacity as u32;
        prepared.header = Some([capacity as u32, (directory_offset / 16) as u32, 0, 0]);
        prepared.directory_offset = directory_offset as usize;
        prepared.identity = Some(identity);
        Ok(prepared)
    }
}
