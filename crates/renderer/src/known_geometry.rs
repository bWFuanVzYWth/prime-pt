//! 四个 16B 对齐顶点属于同一个 quad，槽 q 的顶点从 4*q 开始，BLAS 与命中重建共读。
use super::{Error, Gpu, RegionGpu};
use simple_engine::{scene_cache, vulkan::native::Buffer};

impl Gpu {
    pub(super) fn prepare_vertices(
        &mut self,
        entries: &[scene_cache::Prepared<RegionGpu>],
        end: u32,
        queued: bool,
    ) -> Result<(Option<Buffer>, u32), Error> {
        if end == 0 || (self.known_geometry.is_some() && end <= self.known_geometry_capacity) {
            return Ok((self.known_geometry, self.known_geometry_capacity));
        }
        let capacity = end
            .checked_next_power_of_two()
            .ok_or(Error::Invalid("顶点槽范围溢出"))?;
        let next = Buffer::new(&mut self.context, u64::from(capacity) * 64, false)?;
        let retained = entries
            .iter()
            .filter_map(|e| match e {
                scene_cache::Prepared::Reused(i) => Some(self.region_cache.entries()[*i].range.end),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        if retained != 0 {
            let old = self.known_geometry.expect("已发布几何持有顶点列");
            if queued {
                self.context
                    .enqueue_copy(old, next, u64::from(retained) * 64)?;
            } else {
                old.copy_prefix_to(&mut self.context, &next, u64::from(retained) * 64)?;
            }
        }
        Ok((Some(next), capacity))
    }
}
