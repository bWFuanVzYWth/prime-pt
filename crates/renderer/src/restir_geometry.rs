//! ReSTIR 的随机交点恢复平面：每槽 48B 原局部四角和 16B region 键，仅该消费者存活时驻留。
//! 最后一个word保存只增不复用的mesh身份；删除/重建在同一发布批次清零旧槽身份。
//! 初次选择需从当前 CPU 场景上传；后续只写新几何与退役身份，容量增长复制旧前缀。

use super::{Error, Gpu, RegionGpu};
use simple_engine::{scene_cache, vulkan::native::Buffer};
use std::ops::Range;

pub(super) struct GeometryUpdate {
    pub buffer: Option<Buffer>,
    data: Vec<[u32; 16]>,
    ranges: Vec<(usize, Range<usize>)>,
    retired: Vec<(usize, u32)>,
}

impl GeometryUpdate {
    pub fn writes(&self) -> Vec<(usize, &[u8])> {
        self.ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&self.data[range.clone()])))
            .chain(
                self.retired
                    .iter()
                    .map(|(offset, value)| (*offset, bytemuck::bytes_of(value))),
            )
            .collect()
    }
    pub fn written_bytes(&self) -> u64 {
        self.data.len() as u64 * 64 + self.retired.len() as u64 * 4
    }
}

impl Gpu {
    pub(super) fn prepare_restir_geometry(
        &mut self,
        source: prime_scene::RenderView<'_>,
        entries: &[scene_cache::Prepared<RegionGpu>],
        end: u32,
    ) -> Result<GeometryUpdate, Error> {
        if self.renderer_kind != prime_scene::RealtimeRenderer::RestirPt {
            return Ok(GeometryUpdate {
                buffer: None,
                data: Vec::new(),
                ranges: Vec::new(),
                retired: Vec::new(),
            });
        }
        let first = self.restir_geometry.is_none();
        let bytes = u64::from(
            end.max(1)
                .checked_next_power_of_two()
                .ok_or(Error::Invalid("ReSTIR几何容量溢出"))?,
        ) * 64;
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
            .filter(|(_, e)| first || matches!(e, scene_cache::Prepared::Built(_)))
            .try_fold(0usize, |n, (r, _)| {
                n.checked_add(r.mesh.len())
                    .ok_or(Error::Invalid("ReSTIR几何数量溢出"))
            })?;
        let mut data = Vec::new();
        data.try_reserve_exact(count)
            .map_err(scene_cache::Error::from)?;
        let mut ranges = Vec::new();
        for (region, entry) in source.regions.iter().zip(entries) {
            if !first && !matches!(entry, scene_cache::Prepared::Built(_)) {
                continue;
            }
            let start = data.len();
            let geometry = entry.get(self.region_cache.entries());
            for quad in region.mesh.quads() {
                let mut row = [0; 16];
                for (index, value) in quad.positions().into_iter().flatten().enumerate() {
                    row[index] = value.to_bits();
                }
                row[12..15].copy_from_slice(&region.key.map(|v| v as u32));
                row[15] = geometry.identity;
                data.push(row);
            }
            let entry = entry.get(self.region_cache.entries());
            ranges.push((entry.range.start as usize * 64, start..data.len()));
        }
        let mut retired = Vec::new();
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
                retired.extend(old.range.clone().map(|slot| (slot as usize * 64 + 60, 0)));
            }
        }
        Ok(GeometryUpdate {
            buffer: Some(buffer),
            data,
            ranges,
            retired,
        })
    }
}
