//! 场景候选准备、增量上传与共同发布；失败保留现有覆盖。

use super::*;

impl Gpu {
    pub(super) fn build_scene(
        context: &mut Context,
        mesh: &Mesh,
    ) -> Result<(Scene, Option<Buffer>), Error> {
        if mesh.quads.len() > 4 * 1024 * 1024 || mesh.quads.iter().any(|q| !q.valid()) {
            return Err(Error::Invalid("quad 非有限或超过 4M 面预算"));
        }
        let n = mesh.quads.len().max(1) as u64;
        let quads =
            Buffer::mapped_device_local(context, n * scene_cache::QUAD_ATTRIBUTE_BYTES as u64)?;
        let tokens = Buffer::mapped_device_local(context, n * 8)?;
        let instance_quad_bases = Buffer::upload(context, &[0; 4])?;
        let (top, vertices) = if mesh.quads.is_empty() {
            (context.acceleration_instances(&[])?, None)
        } else {
            let mut encoder = scene_cache::QuadEncoder::with_capacity(mesh.quads.len())?;
            for q in &mesh.quads {
                encoder.push(
                    q.positions(),
                    0,
                    attribute_update::linear_attributes(q.attributes()),
                )?;
            }
            let owner = scene_cache::Cache::<()>::new()?;
            let mut update = owner.begin(0)?;
            let resident =
                scene_cache::ResidentQuads::prepare(encoder.finish(), None, update.parts_mut().2)?;
            let vertices = Buffer::new(context, n * 64, false)?;
            let input = vulkan::QuadBlasInput {
                quads: &resident,
                opaque: false,
                opacity: None,
                output: vulkan::QuadOutput {
                    attributes: Some(quads),
                    identities: Some(tokens),
                    vertices: Some(vertices),
                    ..Default::default()
                },
            };
            let instance = TriangleInstance {
                geometry: vulkan::BatchBlas::New(0),
                transform: [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]],
                custom_index: 0,
            };
            (
                context.quad_scene_batch(&[input], &[instance])?.top,
                Some(vertices),
            )
        };
        Ok((
            Scene {
                top,
                quads,
                tokens,
                instance_quad_bases,
            },
            vertices,
        ))
    }

    #[cfg(test)]
    /// 无窗口测试的单mesh入口；生产通过region视图发布。
    pub fn set_scene(&mut self, mesh: &Mesh) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_scene_inner(mesh);
        self.collect_resources()?;
        result
    }

    #[cfg(test)]
    pub(super) fn set_scene_inner(&mut self, mesh: &Mesh) -> Result<(), Error> {
        self.cancel_packets()?;
        let (scene, vertices) = Self::build_scene(&mut self.context, mesh)?;
        self.invalidate()?;
        self.known_geometry = vertices;
        self.known_geometry_capacity = mesh.quads.len() as u32;
        self.scene = scene;
        self.region_cache = scene_cache::Cache::new()?;
        self.terrain_uploads.clear();
        self.pending_storage = None;
        self.attribute_revisions.clear();
        Ok(())
    }

    /// 发布已排序、唯一且完整的region借用视图；输入只在调用内有效。
    ///
    /// 宿主须已确认旧命令完成。几何revision决定BLAS，content revision决定属性写入；
    /// 新几何在 GPU 生成属性，内容更新仅写变化范围。扩容时在设备端复制旧数据。
    /// 管理器空发布不触碰GPU；anchor只改变TLAS。准备或映射失败保留旧目录和属性。
    pub fn set_regions(&mut self, source: prime_scene::RenderView<'_>) -> Result<(), Error> {
        self.scene_update = None;
        self.complete_previous()?;
        self.cancel_packets()?;
        let result = self.set_regions_inner(source, false);
        if result.is_ok() && self.scene_update.is_some_and(|timing| !timing.changed) {
            return result;
        }
        if result.is_err() {
            self.collect_resources()?;
        }
        result
    }

    /// 当前可绘制 TLAS 的世界原点。候选移动或上传时相机仍相对这个已发布原点。
    pub const fn scene_anchor(&self) -> [f64; 3] {
        self.anchor
    }

    /// 最近一次成功场景调用的墙钟及工作集；失败时为None，空发布保存零工作量。
    pub const fn scene_timing(&self) -> Option<SceneUpdateTiming> {
        self.scene_update
    }

    /// 按原生队列顺序发布 64³ 地形。变化页经有界通道交予后台编码池，未变分片复用源版本。
    /// 返回 false 时宿主保留管理器发布令牌并继续绘制旧覆盖。生产错误可能在后续领取时返回。
    /// 同一候选分帧传输期间固定其原点，发布后下一次调用可单独移动 TLAS。
    /// GPU 展开、AS、灯树及表补丁在同队列执行。
    /// # Errors
    /// 与同步场景入口相同；失败撤销候选，已排队的独占临时资源按 fence 退役。
    pub fn stream_regions(
        &mut self,
        mut source: prime_scene::RenderView<'_>,
    ) -> Result<bool, Error> {
        self.scene_update = None;
        if source.anchor.iter().any(|v| !v.is_finite()) {
            return Err(Error::Invalid("场景原点须有限"));
        }
        self.complete_previous()?;
        self.cpu_packets.handoff_bytes = 0;
        self.cpu_packets.encoded = 0;
        self.cpu_packets.reused = 0;
        self.terrain_uploads.begin_frame();
        let polling = std::time::Instant::now();
        let worker_ns = self.poll_packets(source)?;
        let handoff = elapsed_ns(polling);
        if self.cpu_packets.pending() {
            self.scene_update = Some(SceneUpdateTiming {
                upload_pending: true,
                geometry_ns: handoff,
                cpu_source_bytes: self.cpu_packets.handoff_bytes as u64,
                worker_geometry_ns: worker_ns,
                instances: source.regions.len(),
                ..Default::default()
            });
            return Ok(false);
        }
        if let Some(storage) = &self.pending_storage
            && storage.matches(source, self.scene.top)
        {
            source.anchor = storage.anchor;
        }
        let result = self.set_regions_inner(source, true);
        if let Some(t) = &mut self.scene_update {
            t.geometry_ns += handoff;
            t.worker_geometry_ns = worker_ns;
            t.build_quads = self.terrain_uploads.built_quads() as u64;
            t.cpu_source_bytes = self.cpu_packets.handoff_bytes as u64;
            t.encoded_quads = self.cpu_packets.encoded;
            t.reused_fragment_quads = self.cpu_packets.reused;
        }
        if result.is_err() || self.terrain_pending() {
            self.collect_resources()?;
        }
        result.map(|()| self.scene_update.is_some_and(|v| !v.upload_pending))
    }

    /// CPU 页或 GPU 上传仍在途。即使当前没有新编辑，宿主也须继续推进并保留旧覆盖。
    pub fn terrain_pending(&self) -> bool {
        self.cpu_packets.pending()
            || self.pending_storage.is_some()
            || self.terrain_uploads.pending()
    }

    /// 设置一帧可传的地形字节，默认 64 MiB。零值或非 4B 对齐返回错误。
    pub fn set_terrain_upload_budget(&mut self, bytes: usize) -> Result<(), Error> {
        self.terrain_uploads.set_limit(bytes)?;
        Ok(())
    }
    pub(super) fn set_regions_inner(
        &mut self,
        source: prime_scene::RenderView<'_>,
        queued: bool,
    ) -> Result<(), Error> {
        if source.generation != self.resource_generation {
            return Err(Error::Invalid("region与材质资源世代不匹配"));
        }
        if source.regions.len() > 1 << 24 {
            return Err(Error::Invalid("region实例数量超过24位身份域"));
        }
        if source.anchor.iter().any(|v| !v.is_finite())
            || source.regions.windows(2).any(|p| p[0].key >= p[1].key)
            || source
                .regions
                .iter()
                .any(|r| r.geometry_revision == 0 || r.content_revision == 0 || r.mesh.is_empty())
        {
            return Err(Error::Invalid("region视图须有序唯一、非空且revision非零"));
        }
        let mut metrics = SceneUpdateTiming {
            instances: source.regions.len(),
            ..Default::default()
        };
        let same_geometry = source.regions.len() == self.region_cache.entries().len()
            && source
                .regions
                .iter()
                .zip(self.region_cache.entries())
                .all(|(r, old)| r.key == old.key && r.geometry_revision == old.revision);
        let same_content = source
            .regions
            .iter()
            .map(|r| (r.key, r.content_revision))
            .eq(self.attribute_revisions.iter().copied());
        if same_geometry
            && same_content
            && source.anchor == self.anchor
            && !self.scene_resources_dirty()
        {
            if self.terrain_uploads.pending() || self.pending_storage.is_some() {
                self.terrain_uploads.clear();
                self.pending_storage = None;
                self.collect_resources()?;
            }
            self.scene_update = Some(metrics);
            return Ok(());
        }
        let mut matches = region_match::regions(
            source.regions,
            self.region_cache.entries(),
            &self.attribute_revisions,
        )?;
        // 同一几何版本可以更换材质。颜色补丁不影响覆盖，其余属性更新只有在旧 BLAS
        // 跳过覆盖时检查是否仍可复用；下面另外核对已绑定 OMM 的 UV／材质身份。
        if !queued {
            for (region, matched) in source.regions.iter().zip(&mut matches) {
                if matched.attributes_dirty
                    && !region.mesh.attribute_only()
                    && matched
                        .reuse
                        .is_some_and(|i| self.region_cache.entries()[i].opaque)
                    && region.mesh.quads().any(|quad| {
                        self.material_coverage
                            .get(quad.material_id().index() as usize)
                            .is_none_or(|&coverage| coverage)
                    })
                {
                    matched.reuse = None;
                }
            }
            for (region, matched) in source.regions.iter().zip(&mut matches) {
                if matched.attributes_dirty
                    && !region.mesh.attribute_only()
                    && self.opacity.map.is_some()
                    && let Some(index) = matched.reuse
                {
                    let actual = region.mesh.quads().enumerate().filter_map(|(i, q)| {
                        let pair = self.opacity.indices(q.material_id().index(), q.uv());
                        (pair != [vulkan::OPACITY_UNKNOWN; 2]).then_some((i as u32, pair))
                    });
                    if !actual.eq(self.region_cache.entries()[index]
                        .opacity_slots
                        .iter()
                        .copied())
                    {
                        matched.reuse = None;
                    }
                }
            }
        }
        let started = std::time::Instant::now();
        let packed = self
            .pending_storage
            .as_mut()
            .filter(|p| queued && p.matches(source, self.scene.top))
            .and_then(|p| p.packed.take())
            .or_else(|| self.cpu_ready.take());
        let PackedRegions {
            mut patches,
            mut update,
            pending,
            opacity_indices,
        } = if let Some(packed) = packed {
            packed
        } else if queued
            && matches
                .iter()
                .any(|m| m.reuse.is_none() || m.attributes_dirty)
        {
            self.start_packets(source, &matches)?;
            self.scene_update = Some(SceneUpdateTiming {
                upload_pending: true,
                geometry_ns: elapsed_ns(started),
                instances: source.regions.len(),
                ..Default::default()
            });
            return Ok(());
        } else {
            let mut update = self.region_cache.begin(source.regions.len())?;
            let (slots, entries, ids) = update.parts_mut();
            let dirty_triangles = source.regions.iter().zip(&matches).try_fold(
                0usize,
                |count, (region, matched)| {
                    if matched.reuse.is_some() {
                        Ok(count)
                    } else {
                        count
                            .checked_add(
                                region
                                    .mesh
                                    .len()
                                    .checked_mul(2)
                                    .ok_or(Error::Invalid("dirty三角数量溢出"))?,
                            )
                            .ok_or(Error::Invalid("dirty三角数量溢出"))
                    }
                },
            )?;
            let mut opacity_indices = Vec::new();
            if self.opacity.map.is_some() {
                opacity_indices
                    .try_reserve_exact(dirty_triangles)
                    .map_err(scene_cache::Error::from)?;
            }
            let mut pending = Vec::with_capacity(source.regions.len());
            for (region, matched) in source.regions.iter().zip(&matches) {
                if let Some(index) = matched.reuse {
                    entries.push(scene_cache::Prepared::Reused(index));
                    continue;
                }
                let count = u32::try_from(region.mesh.len())
                    .map_err(|_| Error::Invalid("region面索引超过u32"))?;
                let range = slots.take(count)?;
                if range.end > self.attribute_slot_limit {
                    return Err(Error::Invalid("region属性槽超过设备缓冲容量"));
                }
                let start = opacity_indices.len();
                let mut encoder = scene_cache::QuadEncoder::with_capacity(count as usize)?;
                let mut opaque = true;
                let mut opacity_slots = Vec::new();
                for (local, (quad, attributes)) in region
                    .mesh
                    .quads()
                    .zip(region.mesh.attributes())
                    .enumerate()
                {
                    let coverage = self
                        .material_coverage
                        .get(quad.material_id().index() as usize)
                        .ok_or(Error::Invalid("构建AS前quad记录或材质身份无效"))?;
                    if !quad.valid() {
                        return Err(Error::Invalid("构建AS前quad记录或材质身份无效"));
                    }
                    opaque &= !coverage;
                    encoder.push(
                        quad.positions(),
                        0,
                        attribute_update::linear_attributes(attributes),
                    )?;
                    if self.opacity.map.is_some() {
                        let pair = self.opacity.indices(quad.material_id().index(), quad.uv());
                        opacity_indices.extend(pair);
                        if pair != [vulkan::OPACITY_UNKNOWN; 2] {
                            opacity_slots
                                .try_reserve(1)
                                .map_err(scene_cache::Error::from)?;
                            opacity_slots.push((local as u32, pair));
                        }
                    }
                }
                pending.push((entries.len(), start..opacity_indices.len()));
                let old = self
                    .region_cache
                    .entries()
                    .binary_search_by_key(&region.key, |e| e.key)
                    .ok()
                    .map(|index| &self.region_cache.entries()[index].resident);
                let resident = scene_cache::ResidentQuads::prepare(encoder.finish(), old, ids)?;
                metrics.triangle_bytes += resident.positions().bytes() as u64;
                metrics.triangle_capacity_bytes += resident.positions().retained_bytes() as u64;

                entries.push(scene_cache::Prepared::Built(RegionGpu {
                    key: region.key,
                    revision: region.geometry_revision,
                    baked_content: region.content_revision,
                    fragments: Vec::new(),
                    lamps: None,
                    range,
                    blas: None,
                    opaque,
                    opacity_slots,
                    resident,
                }));
            }
            PackedRegions {
                patches: Default::default(),
                update,
                pending,
                opacity_indices,
            }
        };
        let (_, entries, _) = update.parts_mut();
        let same_acceleration = same_geometry && pending.is_empty();
        let rebuild_top = !same_acceleration || source.anchor != self.anchor;
        metrics.dirty_regions = pending.len();
        let end = entries
            .iter()
            .map(|e| e.get(self.region_cache.entries()).range.end)
            .max()
            .unwrap_or(0);
        let grow =
            u64::from(end) * scene_cache::QUAD_ATTRIBUTE_BYTES as u64 > self.scene.quads.len();
        let packet_ns = elapsed_ns(started);
        let (quads, tokens, vertex_buffer, vertex_capacity, mut allocation_ns) =
            if let Some(saved) = self
                .pending_storage
                .as_ref()
                .filter(|v| queued && v.matches(source, self.scene.top))
            {
                (saved.quads, saved.tokens, saved.vertices, saved.capacity, 0)
            } else {
                let uploading = std::time::Instant::now();
                let quads = if grow {
                    let capacity = end
                        .max(4096)
                        .checked_next_power_of_two()
                        .ok_or(Error::Invalid("属性容量溢出"))?;
                    let buffer = Buffer::mapped_device_local(
                        &mut self.context,
                        u64::from(capacity) * scene_cache::QUAD_ATTRIBUTE_BYTES as u64,
                    )?;
                    // 旧槽只复制需继承的前缀，完整替换的槽由后续属性写入覆盖。
                    let retained_end = source
                        .regions
                        .iter()
                        .zip(entries.iter())
                        .zip(&matches)
                        .filter(|((region, entry), matched)| {
                            !matches!(entry, scene_cache::Prepared::Built(_))
                                && (!matched.attributes_dirty || region.mesh.attribute_only())
                        })
                        .map(|((_, entry), _)| entry.get(self.region_cache.entries()).range.end)
                        .max()
                        .unwrap_or(0);
                    if retained_end != 0 {
                        metrics.attribute_copy_bytes =
                            u64::from(retained_end) * scene_cache::QUAD_ATTRIBUTE_BYTES as u64;
                        if queued {
                            self.context.enqueue_copy(
                                self.scene.quads,
                                buffer,
                                metrics.attribute_copy_bytes,
                            )?;
                        } else {
                            self.scene.quads.copy_prefix_to(
                                &mut self.context,
                                &buffer,
                                metrics.attribute_copy_bytes,
                            )?;
                        }
                    }
                    buffer
                } else {
                    self.scene.quads
                };

                let tokens = if u64::from(end) * 8 > self.scene.tokens.len() {
                    let buffer = Buffer::mapped_device_local(
                        &mut self.context,
                        quads.len() / scene_cache::QUAD_ATTRIBUTE_BYTES as u64 * 8,
                    )?;
                    let retained = entries
                        .iter()
                        .filter_map(|e| match e {
                            scene_cache::Prepared::Reused(i) => {
                                Some(self.region_cache.entries()[*i].range.end)
                            }
                            _ => None,
                        })
                        .max()
                        .unwrap_or(0);
                    if retained != 0 {
                        if queued {
                            self.context.enqueue_copy(
                                self.scene.tokens,
                                buffer,
                                u64::from(retained) * 8,
                            )?;
                        } else {
                            self.scene.tokens.copy_prefix_to(
                                &mut self.context,
                                &buffer,
                                u64::from(retained) * 8,
                            )?;
                        }
                    }
                    buffer
                } else {
                    self.scene.tokens
                };

                let (vertex_buffer, vertex_capacity) =
                    self.prepare_vertices(entries, end, queued)?;
                let allocation_ns = elapsed_ns(uploading);

                (quads, tokens, vertex_buffer, vertex_capacity, allocation_ns)
            };
        let descriptors = std::time::Instant::now();
        let inputs: Vec<_> = pending
            .iter()
            .map(|(index, range)| {
                let entry = entries[*index].get(self.region_cache.entries());
                vulkan::QuadBlasInput {
                    quads: &entry.resident,
                    output: vulkan::QuadOutput {
                        first: entry.range.start,
                        region: entry.key,
                        attributes: Some(quads),
                        identities: Some(tokens),
                        vertices: vertex_buffer,
                        cells: None,
                    },
                    opaque: entry.opaque,
                    opacity: self
                        .opacity
                        .map
                        .filter(|_| !range.is_empty())
                        .map(|map| (map, &opacity_indices[range.clone()])),
                }
            })
            .collect();
        metrics.geometry_ns = packet_ns + elapsed_ns(descriptors);
        metrics.pending_capacity_bytes =
            (pending.capacity() * size_of::<(usize, std::ops::Range<usize>)>()) as u64;
        metrics.input_capacity_bytes =
            (inputs.capacity() * size_of::<vulkan::QuadBlasInput<'_>>()) as u64;
        let saved_transfer = self
            .pending_storage
            .as_mut()
            .filter(|p| queued && p.matches(source, self.scene.top) && p.anchor == source.anchor)
            .and_then(|p| p.transfer.take());
        let transfer = if let Some(saved) = saved_transfer {
            saved
        } else {
            let packing = std::time::Instant::now();
            let mut baked_lamps = std::collections::BTreeMap::new();
            let mut attributes = Vec::new();
            let dirty_count = source
                .regions
                .iter()
                .zip(entries.iter())
                .zip(&matches)
                .enumerate()
                .filter(|(i, ((_, e), m))| {
                    !patches.contains_key(i)
                        && matches!(e, scene_cache::Prepared::Reused(_))
                        && m.attributes_dirty
                })
                .map(|(_, v)| v)
                .try_fold(0usize, |n, ((r, e), _)| {
                    let partial =
                        !matches!(e, scene_cache::Prepared::Built(_)) && r.mesh.attribute_only();
                    let count = if partial {
                        r.mesh
                            .attribute_ranges()
                            .map(|range| range.quads.len())
                            .sum()
                    } else {
                        r.mesh.len()
                    };
                    n.checked_add(count).ok_or(Error::Invalid("属性数量溢出"))
                })?;
            attributes
                .try_reserve_exact(dirty_count)
                .map_err(scene_cache::Error::from)?;
            let mut ranges = Vec::with_capacity(source.regions.len());
            let mut versions = Vec::with_capacity(source.regions.len());
            for (row, ((region, entry), matched)) in source
                .regions
                .iter()
                .zip(entries.iter())
                .zip(&matches)
                .enumerate()
            {
                if let Some(mut p) = patches.remove(&row) {
                    if let Some(lamps) = p.lamps.take() {
                        baked_lamps.insert(region.key, lamps);
                    }
                    let base = attributes.len();
                    if attributes.is_empty() {
                        attributes = p.attributes;
                    } else {
                        attributes
                            .try_reserve(p.attributes.len())
                            .map_err(scene_cache::Error::from)?;
                        attributes.append(&mut p.attributes);
                    }
                    ranges
                        .try_reserve(p.ranges.len())
                        .map_err(scene_cache::Error::from)?;
                    ranges.extend(
                        p.ranges
                            .into_iter()
                            .map(|(o, r)| (o, base + r.start..base + r.end)),
                    );
                    versions.push((region.key, region.content_revision));
                    continue;
                }
                let built = matches!(entry, scene_cache::Prepared::Built(_));
                let entry = entry.get(self.region_cache.entries());
                let dirty = !built && matched.attributes_dirty;
                if dirty {
                    if !built && region.mesh.attribute_only() {
                        for patch in region.mesh.attribute_ranges() {
                            attribute_update::pack_patch(
                                &entry.range,
                                patch.first,
                                patch.attributes(),
                                &mut attributes,
                                &mut ranges,
                            )?;
                        }
                    } else {
                        // 新几何已在AS输入边界校验；复用几何的普通属性候选仍需检查。
                        if !built {
                            for quad in region.mesh.quads() {
                                if !quad.valid()
                                    || quad.material_id().index() as usize >= self.light_sides.len()
                                {
                                    return Err(Error::Invalid(
                                        "quad记录或材质身份不属于当前资源世代",
                                    ));
                                }
                            }
                        }
                        attribute_update::pack_patch(
                            &entry.range,
                            0,
                            region.mesh.attributes(),
                            &mut attributes,
                            &mut ranges,
                        )?;
                    }
                }
                versions.push((region.key, region.content_revision));
            }
            metrics.attribute_capacity_bytes =
                (attributes.capacity() * size_of::<[[u32; 4]; 3]>()) as u64;
            metrics.attribute_pack_ns += elapsed_ns(packing);
            metrics.geometry_ns += metrics.attribute_pack_ns;
            let light_started = std::time::Instant::now();
            let light_inputs = entries.iter().zip(source.regions).map(|(e, r)| {
                let entry = e.get(self.region_cache.entries());
                let lamps = if let Some(rows) = baked_lamps.get(&r.key) {
                    Some(rows.as_slice())
                } else if entry.baked_content == r.content_revision {
                    entry.lamps.as_deref()
                } else {
                    None
                };
                (entry.range.start, lamps)
            });
            let lights = self.lighting.prepare_backend(
                source,
                light_inputs,
                lighting::Emitters {
                    sides: &self.light_sides,
                    areas: &self.light_areas,
                    weights: &self.light_weights,
                },
                self.has_emission,
                self.resource_generation,
            )?;
            metrics.light_pack_ns = elapsed_ns(light_started);
            metrics.geometry_ns += metrics.light_pack_ns;
            let light_started = std::time::Instant::now();
            let lighting_gpu = if let Some(tables) = &lights.tables {
                if queued {
                    let alloc = |ctx: &mut Context, n: usize| {
                        Buffer::mapped_device_local(ctx, n.max(16) as u64)
                    };
                    LightingGpu {
                        tree: self.lighting_gpu.tree,
                        meta: alloc(&mut self.context, tables.meta.len() * 16)?,
                        regions: alloc(&mut self.context, tables.regions.len() * 16)?,
                        inverse: alloc(&mut self.context, tables.inverse.len() * 16)?,
                    }
                } else {
                    LightingGpu::upload(&mut self.context, Some(tables))?
                }
            } else {
                self.lighting_gpu
            };
            if lights.tables.is_some() {
                metrics.light_upload_ns = elapsed_ns(light_started);
                metrics.light_upload_bytes = lighting_gpu.meta.len()
                    + lighting_gpu.inverse.len()
                    + lighting_gpu.regions.len();
                metrics.resources_ns += metrics.light_upload_ns;
            }
            // 实例身份只引用同序目录；属性槽可独立扩展。anchor变化和纯属性更新复用目录。
            let mut bases = Vec::new();
            if !same_acceleration {
                bases
                    .try_reserve_exact(entries.len())
                    .map_err(scene_cache::Error::from)?;
                bases.extend(
                    entries
                        .iter()
                        .map(|e| e.get(self.region_cache.entries()).range.start),
                );
            }
            let base_write =
                (!bases.is_empty()).then(|| (0, bytemuck::cast_slice(bases.as_slice())));
            let instance_quad_bases = if let Some((_, bytes)) = base_write
                && (bytes.len() as u64 > self.scene.instance_quad_bases.len()
                    || self.scene.instance_quad_bases == self.program_scene.instance_quad_bases)
            {
                let started = std::time::Instant::now();
                let capacity = bytes.len().max(256).next_power_of_two();
                let buffer = Buffer::mapped_device_local(&mut self.context, capacity as u64)?;
                allocation_ns += elapsed_ns(started);
                buffer
            } else {
                self.scene.instance_quad_bases
            };
            let mut columns = scene_upload::Columns::default();

            if queued {
                if let Some((offset, bytes)) = base_write {
                    columns.append(instance_quad_bases, offset, bytes)?;
                }
                if let Some(t) = &lights.tables {
                    columns.append(lighting_gpu.meta, 0, bytemuck::cast_slice(&t.meta))?;
                    columns.append(lighting_gpu.regions, 0, bytemuck::cast_slice(&t.regions))?;
                    columns.append(lighting_gpu.inverse, 0, bytemuck::cast_slice(&t.inverse))?;
                } else {
                    let p = &lights.patches;
                    columns.rows(lighting_gpu.meta, &p.meta)?;
                    columns.rows(lighting_gpu.regions, &p.regions)?;
                    columns.rows(lighting_gpu.inverse, &p.inverse)?;
                }
            }
            Transfer {
                upload: Default::default(),
                attributes,
                ranges,
                versions,
                bases,
                lights,
                lighting_gpu,
                instance_quad_bases,
                columns,
            }
        };
        let Transfer {
            mut upload,
            attributes,
            ranges,
            versions,
            bases,
            lights,
            mut lighting_gpu,
            instance_quad_bases,
            columns,
        } = transfer;
        let mut attribute_source = None;
        let mut side_source = None;
        let mut leaf_source = None;
        if queued {
            let mut parts = Vec::new();
            if !attributes.is_empty() {
                parts.push(bytemuck::cast_slice(attributes.as_slice()));
            }
            let side_index = parts.len();
            if !columns.bytes.is_empty() {
                parts.push(columns.bytes.as_slice());
            }
            let leaf_index = parts.len();
            if let Some(p) = &lights.device
                && !p.leaves.is_empty()
            {
                parts.push(bytemuck::cast_slice(&p.leaves));
            }
            let staged = upload.stage(&mut self.context, &mut self.terrain_uploads, &parts)?;
            let Some(staged) = staged else {
                metrics.attribute_upload_bytes = self.terrain_uploads.transferred() as u64;
                metrics.triangle_bytes = 0;
                metrics.upload_pending = true;
                metrics.terrain_upload_bytes = self.terrain_uploads.transferred() as u64;
                self.pending_storage = Some(PendingStorage::capture(
                    source,
                    self.scene.top,
                    quads,
                    tokens,
                    vertex_buffer,
                    vertex_capacity,
                    PackedRegions {
                        patches,
                        update,
                        pending,
                        opacity_indices,
                    },
                    Transfer {
                        upload,
                        attributes,
                        ranges,
                        versions,
                        bases,
                        lights,
                        lighting_gpu,
                        instance_quad_bases,
                        columns,
                    },
                )?);
                self.scene_update = Some(metrics);
                return Ok(());
            };
            if !columns.bytes.is_empty() {
                side_source = Some(staged[side_index]);
            }
            if !attributes.is_empty() {
                attribute_source = Some(staged[0]);
            }
            leaf_source = staged.get(leaf_index).copied();
        }
        let top = if rebuild_top {
            let packing = std::time::Instant::now();
            let mut instances = Vec::new();
            instances
                .try_reserve_exact(entries.len())
                .map_err(scene_cache::Error::from)?;
            let mut next_blas = 0;
            for (index, entry) in entries.iter().enumerate() {
                let built = matches!(entry, scene_cache::Prepared::Built(_));
                let entry = entry.get(self.region_cache.entries());
                let geometry = if built {
                    let geometry = vulkan::BatchBlas::New(next_blas);
                    next_blas += 1;
                    geometry
                } else {
                    vulkan::BatchBlas::Existing(
                        entry.blas.ok_or(Error::Invalid("未完成region BLAS"))?,
                    )
                };
                let offset: [f32; 3] = std::array::from_fn(|axis| {
                    (f64::from(entry.key[axis]) * 64.0 - source.anchor[axis]) as f32
                });
                instances.push(TriangleInstance {
                    geometry,
                    transform: [
                        [1., 0., 0., offset[0]],
                        [0., 1., 0., offset[1]],
                        [0., 0., 1., offset[2]],
                    ],
                    custom_index: index as u32,
                });
            }
            metrics.instance_capacity_bytes =
                (instances.capacity() * size_of::<TriangleInstance<vulkan::BatchBlas>>()) as u64;
            let instance_ns = elapsed_ns(packing);
            metrics.attribute_pack_ns += instance_ns;
            metrics.geometry_ns += instance_ns;
            let accelerating = std::time::Instant::now();
            let batch = if queued {
                let allowance = self.terrain_uploads.remaining();
                let submitted = self.context.stream_quad_scene(
                    &mut self.terrain_uploads,
                    allowance,
                    &inputs,
                    &instances,
                )?;
                metrics.triangle_bytes = self.terrain_uploads.transferred() as u64;
                let Some(batch) = submitted else {
                    metrics.upload_pending = true;
                    metrics.terrain_upload_bytes = self.terrain_uploads.transferred() as u64;
                    self.pending_storage = Some(PendingStorage::capture(
                        source,
                        self.scene.top,
                        quads,
                        tokens,
                        vertex_buffer,
                        vertex_capacity,
                        PackedRegions {
                            patches,
                            update,
                            pending,
                            opacity_indices,
                        },
                        Transfer {
                            upload,
                            attributes,
                            ranges,
                            versions,
                            bases,
                            lights,
                            lighting_gpu,
                            instance_quad_bases,
                            columns,
                        },
                    )?);
                    self.scene_update = Some(metrics);
                    return Ok(());
                };
                batch
            } else {
                self.context.quad_scene_batch(&inputs, &instances)?
            };
            metrics.acceleration_ns = elapsed_ns(accelerating);
            metrics.tlas_ns = batch.tlas_prepare_ns;
            metrics.as_submit_wait_ns = batch.blas.timing.submit_wait_ns;
            if !inputs.is_empty() {
                metrics.batch = batch.blas.timing;
            }
            drop(inputs);
            for ((index, _), blas) in pending.iter().zip(batch.blas.blases) {
                let scene_cache::Prepared::Built(entry) = &mut entries[*index] else {
                    unreachable!("dirty输入对应新记录");
                };
                entry.blas = Some(blas);
            }
            batch.top
        } else {
            self.scene.top
        };
        drop(pending);
        if let Some(proposal) = &lights.device {
            use lighting::LightChunk;
            use vulkan::native::{LightRegionInput, LightWorldInput};
            let previous = self.context.light_regions(self.lighting_gpu.tree)?;
            let mut regions =
                simple_engine::light_sampling::registry::reserved(proposal.chunks.len())?;
            regions.extend(proposal.chunks.iter().map(|entry| match *entry {
                LightChunk::Reuse(index) => LightRegionInput::Reuse(previous[index]),
                LightChunk::Build { grid, count } => LightRegionInput::Build { grid, count },
            }));
            let input = LightWorldInput {
                vertices: vertex_buffer.unwrap_or(self.geometry_placeholder),
                leaves: &proposal.leaves,
                regions: &regions,
                records: false,
                source: leaf_source,
            };
            lighting_gpu.tree = if queued {
                self.context.enqueue_light_world(input)?
            } else {
                self.context.build_light_world(input)?
            };
            metrics.light_upload_bytes += size_of_val(proposal.leaves.as_slice()) as u64;
        }
        let geometry_buffer = vertex_buffer.unwrap_or(self.geometry_placeholder);
        let bindings_changed = vertex_buffer != self.known_geometry
            || top != self.scene.top
            || tokens != self.scene.tokens
            || quads != self.scene.quads
            || instance_quad_bases != self.scene.instance_quad_bases
            || lighting_gpu != self.lighting_gpu;
        if bindings_changed {
            let collecting = std::time::Instant::now();
            let mut candidates = lighting_gpu.roots().to_vec();
            candidates.extend([
                vulkan::native::Resource::from(top),
                quads.into(),
                tokens.into(),
                instance_quad_bases.into(),
                geometry_buffer.into(),
            ]);
            candidates.extend(side_source.map(vulkan::native::Resource::from));
            candidates.extend(attribute_source.map(vulkan::native::Resource::from));
            self.collect_with(&candidates)?;
            metrics.collect_ns = elapsed_ns(collecting);
        }
        metrics.resources_ns += metrics.collect_ns;
        // 目录独占校验之后只有一次原子映射写入；成功复制后发布阶段不再失败。
        let publication = self
            .region_cache
            .prepare(update, |entry| entry.range.clone())?;
        let light_publication = self.lighting.publication(lights)?;

        let patches = light_publication.patches();
        fn rows(values: &[(usize, [u32; 4])]) -> Vec<(usize, &[u8])> {
            values
                .iter()
                .map(|(row, value)| (row * 16, bytemuck::bytes_of(value)))
                .collect()
        }
        let meta_writes = rows(&patches.meta);
        let inverse_writes = rows(&patches.inverse);
        let region_writes = rows(&patches.regions);
        let light_patch_bytes =
            (meta_writes.len() + inverse_writes.len() + region_writes.len()) as u64 * 16;

        let writes: Vec<_> = ranges
            .iter()
            .map(|(offset, range)| (*offset, bytemuck::cast_slice(&attributes[range.clone()])))
            .collect();
        let base_write = (!bases.is_empty()).then(|| (0, bytemuck::cast_slice(bases.as_slice())));
        let writes_attributes = !writes.is_empty() || base_write.is_some();
        if writes_attributes || light_patch_bytes != 0 || side_source.is_some() {
            let writing = std::time::Instant::now();
            let patches: &[(&Buffer, &vulkan::native::BufferWriteRanges<'_>)] = &[
                (&quads, &writes),
                (&instance_quad_bases, base_write.as_slice()),
                (&lighting_gpu.meta, &meta_writes),
                (&lighting_gpu.inverse, &inverse_writes),
                (&lighting_gpu.regions, &region_writes),
            ];
            if queued {
                if let Some(buffer) = attribute_source {
                    let copies: Vec<_> = ranges
                        .iter()
                        .map(|(to, r)| {
                            (
                                quads,
                                (r.start * scene_cache::QUAD_ATTRIBUTE_BYTES) as u64,
                                *to as u64,
                                (r.len() * scene_cache::QUAD_ATTRIBUTE_BYTES) as u64,
                            )
                        })
                        .collect();
                    self.context.enqueue_buffer_ranges(buffer, &copies)?;
                }
                if let Some(buffer) = side_source {
                    self.context
                        .enqueue_buffer_ranges(buffer, &columns.copies)?;
                }
            } else {
                Buffer::write_scattered_batches(&mut self.context, patches)?;
            }
            // 共同映射/复制的墙钟只归因一次，不能按buffer把同一等待重复累加。
            let wall = elapsed_ns(writing);
            if !writes_attributes {
                metrics.light_upload_ns += wall;
            } else {
                metrics.attribute_upload_ns = wall;
            }
            metrics.light_upload_bytes += light_patch_bytes;
            if !writes_attributes {
                metrics.resources_ns += wall;
            }
        }
        metrics.attribute_upload_ns += allocation_ns;
        metrics.attribute_upload_bytes = (attributes.len() * scene_cache::QUAD_ATTRIBUTE_BYTES
            + base_write.map_or(0, |(_, bytes)| bytes.len()))
            as u64;
        metrics.resources_ns += metrics.attribute_upload_ns;
        let publishing = std::time::Instant::now();
        publication.commit();
        light_publication.commit();
        self.lighting.set_epoch(self.resource_generation);
        self.lighting_gpu = lighting_gpu;
        self.lighting_dirty = false;
        self.scene = Scene {
            top,
            quads,
            tokens,
            instance_quad_bases,
        };
        if matches
            .iter()
            .zip(source.regions)
            .any(|(m, r)| m.attributes_dirty && !r.mesh.attribute_only())
        {
            self.terrain_light.request_refresh();
        }
        self.known_geometry = vertex_buffer;
        self.known_geometry_capacity = vertex_capacity;

        self.anchor = source.anchor;
        self.attribute_revisions = versions;
        if bindings_changed {
            self.renderer = None;
            self.guide_pipeline = None;
        }
        self.transition_history(history::Event::Accumulation(
            history::AccumulationEvent::SceneChanged,
        ))?;
        metrics.publish_ns = elapsed_ns(publishing);
        metrics.resources_ns += metrics.publish_ns;
        metrics.changed = true;
        metrics.terrain_upload_bytes = if queued {
            self.terrain_uploads.transferred() as u64
        } else {
            metrics.batch.input_bytes
                + metrics.attribute_upload_bytes
                + metrics.light_upload_bytes
                + if rebuild_top {
                    metrics.instances as u64 * 64
                } else {
                    0
                }
        };
        self.terrain_uploads.clear();
        self.pending_storage = None;
        self.scene_update = Some(metrics);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn set_sections(&mut self, source: &prime_world::Scene) -> Result<(), Error> {
        let regions: Vec<_> = source.render_regions().collect();
        self.set_regions(prime_scene::RenderView {
            generation: source.generation(),
            regions: &regions,
            anchor: source.anchor(),
        })
    }

    // 单场景和两个视图的固定根；录制命令另由Context保护短命绑定。
    pub(super) fn refresh_shadow_scene(&mut self) -> Result<(), Error> {
        let key = (
            self.features.top.unwrap_or(self.scene.top),
            self.scene.quads,
            self.scene.instance_quad_bases,
            self.atlas,
            self.material_gpu.table,
            self.features.surfaces,
            self.features.pixels,
        );
        if key != self.shadow_scene {
            self.shadow_resolver = self.terrain_light.resolver(
                &mut self.context,
                Some(self.shadow_resolver),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/shadow_columns.spv")),
                    c"shadow_columns",
                ),
                key.0,
                &[
                    Binding::StorageBuffer(key.1),
                    Binding::StorageBuffer(key.2),
                    Binding::SampledImage(key.3),
                    Binding::StorageBuffer(key.4),
                    Binding::StorageBuffer(key.5),
                    Binding::StorageBuffer(key.6),
                ],
            )?;
            self.shadow_scene = key;
            self.terrain_light.request_refresh();
        }
        Ok(())
    }

    pub(super) fn collect_resources(&mut self) -> Result<(), Error> {
        self.refresh_shadow_scene()?;
        self.collect_with(&[])
    }

    pub(super) fn collect_with(&mut self, extra: &[vulkan::native::Resource]) -> Result<(), Error> {
        let mut roots = vec![
            self.scene.top.into(),
            self.scene.quads.into(),
            self.scene.tokens.into(),
            self.scene.instance_quad_bases.into(),
            self.program_scene.top.into(),
            self.program_scene.quads.into(),
            self.program_scene.tokens.into(),
            self.program_scene.instance_quad_bases.into(),
            self.atlas.into(),
            self.energy.into(),
            self.nearest.into(),
            self.linear.into(),
        ];
        roots.extend([
            vulkan::native::Resource::from(self.material_gpu.table),
            self.material_gpu.mers.into(),
            self.material_gpu.auxiliary.into(),
        ]);
        roots.push(self.material_gpu.emission_alias.into());
        roots.extend(self.opacity.map.map(vulkan::native::Resource::from));
        roots.extend(self.lighting_gpu.roots());
        roots.extend(self.program_lighting.roots());
        roots.extend(self.atmosphere.roots());
        roots.extend(self.program_view.roots());
        roots.extend(self.bloom_filter.roots());
        if let Some(view) = &self.view {
            roots.extend(view.roots());
        }
        if let Some(generation) = &self.generation {
            roots.extend(generation.roots());
        }
        if let Some(renderer) = &self.renderer {
            roots.extend(renderer.roots());
        }
        roots.extend(self.guide_pipeline.map(vulkan::native::Resource::from));
        roots.extend(self.terrain_light.roots());
        roots.push(self.terrain_input.into());
        roots.push(self.shadow_resolver.into());
        roots.extend([self.geometry_placeholder].map(vulkan::native::Resource::from));
        roots.extend(self.known_geometry.map(vulkan::native::Resource::from));

        #[cfg(test)]
        roots.extend(self.test_roots.iter().copied());
        roots.extend(self.terrain_uploads.roots());
        if let Some(v) = &self.pending_storage {
            roots.extend([v.quads, v.tokens].map(vulkan::native::Resource::from));
            roots.extend(v.vertices.map(vulkan::native::Resource::from));
            if let Some(t) = &v.transfer {
                roots.extend(t.upload.roots());
                roots.extend(t.lighting_gpu.roots());
                roots.push(t.instance_quad_bases.into());
            }
        }
        self.features.roots(&mut roots);
        roots.extend_from_slice(extra);
        self.context.collect(&roots)?;
        Ok(())
    }
}
