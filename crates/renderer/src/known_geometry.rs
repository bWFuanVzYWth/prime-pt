//! 当前首表面的精确四角与 region 坐标，供照明和导引从共享命中重建表面。
//! 每 quad 64B，首 16B 保留；只上传重建的 region，复用的 BLAS 与位置数据一起保留。
//! 更新在场景事务内共同映射和发布，不存储跨帧路径身份或活动目录。
use super::{Error, Gpu, RegionGpu};
use simple_engine::{scene_cache, vulkan::native::Buffer};
use std::ops::Range;

pub(super) struct GeometryUpdate {
    pub buffer: Option<Buffer>,
    pub capacity: u32,

    data: Vec<[u32; 16]>,
    ranges: Vec<(usize, Range<usize>)>,
}
impl GeometryUpdate {
    pub fn writes(&self) -> Vec<(usize, &[u8])> {
        self.ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&self.data[range.clone()])))
            .collect()
    }
    pub fn written_bytes(&self) -> u64 {
        self.data.len() as u64 * 64
    }
}
impl Gpu {
    pub(super) fn prepare_known_geometry(
        &mut self,
        source: prime_scene::RenderView<'_>,
        entries: &[scene_cache::Prepared<RegionGpu>],
        end: u32,
    ) -> Result<GeometryUpdate, Error> {
        let mut prepared = GeometryUpdate {
            buffer: self.known_geometry,
            capacity: self.known_geometry_capacity,

            data: Vec::new(),
            ranges: Vec::new(),
        };
        if !self.needs_known_geometry() {
            prepared.buffer = None;
            prepared.capacity = 0;
            return Ok(prepared);
        }
        let first = self.known_geometry.is_none();
        if !first
            && entries
                .iter()
                .all(|entry| matches!(entry, scene_cache::Prepared::Reused(_)))
        {
            return Ok(prepared);
        }
        let capacity = end
            .max(1)
            .checked_next_power_of_two()
            .ok_or(Error::Invalid("首面几何容量溢出"))?
            .max(self.known_geometry_capacity);
        let bytes = 16 + u64::from(capacity) * 64;
        let buffer = if let Some(old) = self.known_geometry
            && old.len() >= bytes
        {
            old
        } else {
            let next = Buffer::mapped_device_local(&mut self.context, bytes)?;
            if let Some(old) = self.known_geometry {
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
                    .ok_or(Error::Invalid("首面几何数量溢出"))
            })?;
        prepared
            .data
            .try_reserve_exact(count)
            .map_err(scene_cache::Error::from)?;
        prepared
            .ranges
            .try_reserve_exact(entries.len())
            .map_err(scene_cache::Error::from)?;
        for (region, entry) in source.regions.iter().zip(entries) {
            if !first && matches!(entry, scene_cache::Prepared::Reused(_)) {
                continue;
            }
            let start = prepared.data.len();
            for (_, mesh) in region.mesh.fragments() {
                for quad in &mesh.quads {
                    let mut row = [0; 16];
                    for (index, value) in quad.positions().into_iter().flatten().enumerate() {
                        row[index] = value.to_bits();
                    }
                    row[12..15].copy_from_slice(&region.key.map(|value| value as u32));
                    prepared.data.push(row);
                }
            }
            prepared.ranges.push((
                16 + entry.get(self.region_cache.entries()).range.start as usize * 64,
                start..prepared.data.len(),
            ));
        }
        prepared.buffer = Some(buffer);
        prepared.capacity = capacity;
        Ok(prepared)
    }
}
