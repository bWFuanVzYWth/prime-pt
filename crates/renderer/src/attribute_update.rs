//! 纯颜色发布直接定位变化region；不构造完整场景目录或扫描几何、光源。
use super::{Error, Gpu, SceneUpdateTiming, elapsed_ns};
use prime_scene::RenderChange;
use simple_engine::{scene_cache, vulkan};

/// CPU源保持sRGB；只在派生GPU属性的边界解码共享tint，三个通道仍各占一个FP32槽。
pub(super) fn linear_attributes(
    mut value: scene_cache::QuadAttributes,
) -> scene_cache::QuadAttributes {
    let rgb: [f32; 3] = std::array::from_fn(|i| f32::from_bits(value[2][i]).clamp(0.0, 1.0));
    let linear = simple_engine::color::SrgbRgba::new(rgb[0], rgb[1], rgb[2], 1.0)
        .to_linear_srgb()
        .to_array()
        .map(f32::to_bits);
    value[2][..3].copy_from_slice(&linear[..3]);
    value
}

/// 将区域内连续的源 quad 属性写入同序 GPU 槽，相邻的写入合为一个范围。
pub(super) fn pack_patch(
    slots: &std::ops::Range<u32>,
    first: usize,
    values: impl ExactSizeIterator<Item = scene_cache::QuadAttributes>,
    out: &mut Vec<scene_cache::QuadAttributes>,
    ranges: &mut Vec<(usize, std::ops::Range<usize>)>,
) -> Result<(), Error> {
    let count = values.len();
    if first.checked_add(count).is_none_or(|end| end > slots.len()) {
        return Err(Error::Invalid("属性补丁越界"));
    }
    if count == 0 {
        return Ok(());
    }
    out.try_reserve(count).map_err(scene_cache::Error::from)?;
    let dest = (slots.start as usize + first) * size_of::<scene_cache::QuadAttributes>();
    let start = out.len();
    if let Some((old, range)) = ranges.last_mut()
        && *old + range.len() * size_of::<scene_cache::QuadAttributes>() == dest
        && range.end == start
    {
        range.end += count;
    } else {
        ranges.try_reserve(1).map_err(scene_cache::Error::from)?;
        ranges.push((dest, start..start + count));
    }
    out.extend(values.map(linear_attributes));
    Ok(())
}

impl Gpu {
    /// 尝试共同发布有序唯一的纯tint变化与其内容版本。
    ///
    /// 与完整场景相同，宿主须结束前次GPU使用。世代错误直接返回错误；几何、anchor、
    /// 新增或实际删除需要完整TLAS事务时返回false且不写入。支持的变化全部校验、准备后
    /// 一次写入，成功后CPU版本共同提交；工作量取决于变化项及颜色范围。
    /// 调用者维护revision身份：同geometry revision引用同一已发布几何，颜色overlay的
    /// 基础属性须来自其对应已发布region，不能以相同revision替换UV或材质。
    pub fn try_attribute_changes(
        &mut self,
        generation: u64,
        anchor: [f64; 3],
        changes: &[RenderChange<'_>],
    ) -> Result<bool, Error> {
        self.scene_update = None;
        if self.terrain_pending() {
            return Ok(false);
        }
        if generation != self.resource_generation {
            return Err(Error::Invalid("属性变化与材质资源世代不匹配"));
        }
        let key = |change: &RenderChange<'_>| match change {
            RenderChange::Upsert(region) => region.key,
            RenderChange::Remove(key) => *key,
        };
        if changes
            .windows(2)
            .any(|pair| key(&pair[0]) >= key(&pair[1]))
        {
            return Err(Error::Invalid("属性变化须按region有序唯一"));
        }
        if anchor != self.anchor || self.scene_resources_dirty() {
            return Ok(false);
        }
        // 在写入打包前核算整个批，避免超额的纯 tint 在两个入口各打包一次。
        let bytes = changes.iter().try_fold(0usize, |n, change| {
            let RenderChange::Upsert(r) = change else {
                return None;
            };
            if !r.mesh.attribute_only() {
                return None;
            }
            r.mesh.attribute_ranges().try_fold(n, |n, p| {
                n.checked_add(
                    p.quads
                        .len()
                        .checked_mul(scene_cache::QUAD_ATTRIBUTE_BYTES)?,
                )
            })
        });
        if bytes.is_none_or(|v| {
            v > self
                .terrain_uploads
                .limit()
                .min(256 * scene_cache::QUAD_ATTRIBUTE_BYTES)
        }) {
            return Ok(false);
        }
        let started = std::time::Instant::now();
        let mut attributes = Vec::new();
        let mut ranges = Vec::new();
        let mut versions = Vec::new();
        let mut ack_changes = Vec::new();
        versions
            .try_reserve(changes.len())
            .map_err(scene_cache::Error::from)?;
        for change in changes {
            let region = match change {
                RenderChange::Remove(key) => {
                    if self
                        .region_cache
                        .entries()
                        .binary_search_by_key(key, |old| old.key)
                        .is_ok()
                    {
                        return Ok(false);
                    }
                    continue;
                }
                RenderChange::Upsert(region) => region,
            };
            if region.geometry_revision == 0
                || region.content_revision == 0
                || region.mesh.is_empty()
            {
                return Err(Error::Invalid("region属性变化须非空且revision非零"));
            }
            let Ok(index) = self
                .region_cache
                .entries()
                .binary_search_by_key(&region.key, |old| old.key)
            else {
                return Ok(false);
            };
            let old = &self.region_cache.entries()[index];
            if old.revision != region.geometry_revision || old.range.len() != region.mesh.len() {
                return Ok(false);
            }
            let Ok(stamp) = self
                .attribute_revisions
                .binary_search_by_key(&region.key, |entry| entry.0)
            else {
                return Ok(false);
            };
            if self.attribute_revisions[stamp].1 == region.content_revision {
                continue;
            }
            if !region.mesh.attribute_only() {
                return Ok(false);
            }
            for patch in region.mesh.attribute_ranges() {
                pack_patch(
                    &old.range,
                    patch.first,
                    patch.attributes(),
                    &mut attributes,
                    &mut ranges,
                )?;
            }
            versions.push((stamp, region.content_revision));
            ack_changes
                .try_reserve(1)
                .map_err(scene_cache::Error::from)?;
            ack_changes.push(RenderChange::Upsert(*region));
        }
        let mut metrics = SceneUpdateTiming {
            instances: self.region_cache.entries().len(),
            ..Default::default()
        };
        metrics.attribute_pack_ns = elapsed_ns(started);
        metrics.geometry_ns = metrics.attribute_pack_ns;
        metrics.attribute_capacity_bytes =
            (attributes.capacity() * size_of::<scene_cache::QuadAttributes>()) as u64;
        let writes: Vec<_> = ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&attributes[range.clone()])))
            .collect();
        if self.terrain_uploads.pending()
            || attributes
                .len()
                .saturating_mul(scene_cache::QUAD_ATTRIBUTE_BYTES)
                > self.terrain_uploads.limit()
        {
            return Ok(false);
        }
        self.complete_previous()?;
        let ack = self.lighting.prepare_attribute_ack(&ack_changes)?;
        if !writes.is_empty() {
            let started = std::time::Instant::now();
            vulkan::native::Buffer::write_scattered_batches(
                &mut self.context,
                &[(&self.scene.quads, &writes)],
            )?;
            metrics.attribute_upload_ns = elapsed_ns(started);
            metrics.attribute_upload_bytes =
                (attributes.len() * scene_cache::QUAD_ATTRIBUTE_BYTES) as u64;
            metrics.terrain_upload_bytes = metrics.attribute_upload_bytes;
            metrics.resources_ns = metrics.attribute_upload_ns;
        }
        let started = std::time::Instant::now();
        ack.commit();
        metrics.changed = !versions.is_empty();
        for (index, revision) in versions {
            self.attribute_revisions[index].1 = revision;
        }
        if metrics.changed {
            self.transition_history(super::history::Event::Accumulation(
                super::history::AccumulationEvent::SceneChanged,
            ))?;
        }
        metrics.publish_ns = elapsed_ns(started);
        metrics.resources_ns += metrics.publish_ns;
        self.scene_update = Some(metrics);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn derived_tint_preserves_sub_byte_precision() {
        let tint = [0.001_f32, 0.003, 0.004];
        let mut source = [[0; 4]; 3];
        source[2][..3].copy_from_slice(&tint.map(f32::to_bits));
        let derived = super::linear_attributes(source);
        let linear: [f32; 3] = std::array::from_fn(|i| f32::from_bits(derived[2][i]));
        let restored =
            simple_engine::color::LinearSrgbRgba::new(linear[0], linear[1], linear[2], 1.0)
                .to_srgb()
                .to_array();
        for i in 0..3 {
            assert!((restored[i] - tint[i]).abs() < 1e-7);
        }
    }
}
