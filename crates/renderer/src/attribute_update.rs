//! 纯颜色发布直接定位变化region；不构造完整场景目录或扫描几何、光源。
use super::{Error, Gpu, SceneUpdateTiming, elapsed_ns};
use prime_scene::RenderChange;
use simple_engine::{scene_cache, vulkan};

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
                attributes
                    .try_reserve(patch.quads.len())
                    .map_err(scene_cache::Error::from)?;
                ranges.try_reserve(1).map_err(scene_cache::Error::from)?;
                let first = attributes.len();
                attributes.extend(patch.attributes());
                ranges.push((
                    (old.range.start as usize + patch.first) * 48,
                    first..attributes.len(),
                ));
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
            (attributes.capacity() * size_of::<[[u32; 4]; 3]>()) as u64;
        let writes: Vec<_> = ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&attributes[range.clone()])))
            .collect();
        self.complete_previous()?;
        let ack = self.lighting.prepare_attribute_ack(&ack_changes)?;
        if !writes.is_empty() {
            let started = std::time::Instant::now();
            vulkan::native::Buffer::write_scattered_batches(
                &mut self.context,
                &[(&self.scene.quads, &writes)],
            )?;
            metrics.attribute_upload_ns = elapsed_ns(started);
            metrics.attribute_upload_bytes = (attributes.len() * 48) as u64;
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
        self.restir_dirty |= metrics.changed;
        self.scene_update = Some(metrics);
        Ok(true)
    }
}
