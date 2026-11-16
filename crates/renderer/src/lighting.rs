//! 发光实例的增量目录；静态texel alias属于资源世代，运行期只注册活动quad。
use light_sampling::registry;
use simple_engine::light_sampling;

use std::sync::atomic::{AtomicU64, Ordering};

use prime_scene::{RenderChange, RenderRegion, RenderView};

use crate::gpu::Error;
pub(super) use registry::Record;
use registry::{Registry, Writer, reserved};
pub(super) use registry::{TablePatches, Tables};

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// 同资源世代的只读发光提案；静态亮度忽略动态tint/动画当前帧，不重解码材质。
#[derive(Clone, Copy)]
pub(super) struct Emitters<'a> {
    pub sides: &'a [u32],
    pub areas: &'a [[u32; 4]],
    pub weights: &'a [f32],
}

#[derive(Clone, Copy)]
struct Stamp {
    key: [i32; 3],
    geometry: u64,
    content: u64,
    base: u32,
    quads: usize,
}

struct RegionLights {
    stamp: Stamp,
    ids: Vec<u32>,
}

enum PreparedRegion {
    Reused { index: usize, stamp: Stamp },
    Built(RegionLights),
}

struct Addition {
    entry: usize,
    position: usize,
    record: Record,
}

pub(super) struct Lighting {
    regions: Vec<RegionLights>,
    registry: Registry,
    owner: u64,
    version: u64,
    epoch: u64,
    possible: bool,
}

struct LightingUpdate {
    owner: u64,
    version: u64,
    previous_epoch: u64,
    epoch: u64,
    possible: bool,
    entries: Vec<PreparedRegion>,
    next: Vec<RegionLights>,
    registry: registry::Delta,
}

pub(super) struct Preparation {
    update: LightingUpdate,
    pub tables: Option<Tables>,
    pub patches: TablePatches,
    pub device: Option<registry::DeviceProposal>,
}

/// 独占已校验的CPU目录；GPU原子写成功后提交，丢弃则旧目录不变。
pub(super) struct LightingPublication<'a> {
    lighting: &'a mut Lighting,
    prepared: Preparation,
    version: u64,
}

impl LightingPublication<'_> {
    /// 此阶段独占的离散GPU写入源；设备一次原子写成功后才能提交。
    pub fn patches(&self) -> &TablePatches {
        &self.prepared.patches
    }

    /// 只移动已准备记录、写入稀疏行与版本；不分配、不返回错误。
    pub fn commit(self) {
        let Self {
            lighting,
            prepared,
            version,
        } = self;
        let Preparation {
            mut update,
            tables,
            patches: _,
            device: _,
        } = prepared;
        let mut current = std::mem::take(&mut lighting.regions)
            .into_iter()
            .enumerate()
            .peekable();
        for entry in update.entries {
            let region = match entry {
                PreparedRegion::Built(region) => region,
                PreparedRegion::Reused { index, stamp } => {
                    while current.peek().is_some_and(|(old, _)| *old < index) {
                        current.next();
                    }
                    let mut region = current.next().expect("准备阶段保持有序复用身份").1;
                    region.stamp = stamp;
                    region
                }
            };
            update.next.push(region);
        }
        lighting.regions = update.next;
        lighting.registry.commit(update.registry, tables);
        lighting.epoch = update.epoch;
        lighting.possible = update.possible;
        lighting.version = version;
    }
}

/// 仅颜色的内容版本确认；借用覆盖设备写入，提交不扫描quad或灯表。
pub(super) struct AttributeAck<'a> {
    lighting: &'a mut Lighting,
    revisions: Vec<(usize, u64)>,
    version: u64,
}

impl AttributeAck<'_> {
    /// 设备原子写成功后仅确认已校验的内容版本；不分配。
    pub fn commit(self) {
        for (index, revision) in self.revisions {
            self.lighting.regions[index].stamp.content = revision;
        }
        self.lighting.version = self.version;
    }
}

impl Lighting {
    pub fn new() -> Result<Self, Error> {
        let owner = NEXT_OWNER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Error::Invalid("发光目录所有者身份耗尽"))?;
        Ok(Self {
            regions: Vec::new(),
            registry: Registry::new(64.0)?,
            owner,
            version: 0,
            epoch: 0,
            possible: false,
        })
    }

    /// 与有序region输入线性合并，仅变化几何枚举quad；颜色覆盖只更新stamp。
    /// 同epoch须对应同一份已准备材质side/area表，prepare失败不修改当前分布。
    #[cfg(test)]
    pub fn prepare(
        &self,
        source: RenderView<'_>,
        bases: &[u32],
        emitters: Emitters<'_>,
        possible: bool,
        epoch: u64,
    ) -> Result<Preparation, Error> {
        self.prepare_backend(
            source,
            bases.iter().map(|&base| (base, None)),
            emitters,
            possible,
            epoch,
            false,
        )
    }
    pub fn prepare_backend<'a>(
        &self,
        source: RenderView<'_>,
        regions: impl ExactSizeIterator<Item = (u32, Option<&'a [Record]>)>,
        emitters: Emitters<'_>,
        possible: bool,
        epoch: u64,
        device: bool,
    ) -> Result<Preparation, Error> {
        if source.regions.len() != regions.len() {
            return Err(Error::Invalid("发光region与属性槽目录长度不匹配"));
        }
        let mut entries = reserved(source.regions.len())?;
        let next = reserved(source.regions.len())?;
        let mut writer = Writer::new(&self.registry);
        let mut additions = Vec::new();
        let mut old_index = 0;
        let mut previous = None;
        let same_resources = self.epoch == epoch && self.possible == possible;
        for (region, (base, cached)) in source.regions.iter().zip(regions) {
            if previous.is_some_and(|key| key >= region.key) {
                return Err(Error::Invalid("发光region键须唯一且有序"));
            }
            previous = Some(region.key);
            while self
                .regions
                .get(old_index)
                .is_some_and(|old| old.stamp.key < region.key)
            {
                remove_region(&mut writer, &self.regions[old_index])?;
                old_index += 1;
            }
            let old = self
                .regions
                .get(old_index)
                .filter(|old| old.stamp.key == region.key)
                .map(|_| old_index);
            if old.is_some() {
                old_index += 1;
            }
            let stamp = Stamp {
                key: region.key,
                geometry: region.geometry_revision,
                content: region.content_revision,
                base,
                quads: region.mesh.len(),
            };
            if let Some(index) = old {
                let old = &self.regions[index];
                if stamp.content < old.stamp.content || stamp.geometry < old.stamp.geometry {
                    return Err(Error::Invalid("发光region版本不可回退"));
                }
                let same_content = stamp.content == old.stamp.content;
                let attributes = region.mesh.attribute_only();
                if same_resources && (same_content || attributes) {
                    if stamp.geometry != old.stamp.geometry || stamp.quads != old.stamp.quads {
                        return Err(Error::Invalid("属性覆盖改变了发光几何身份"));
                    }
                    if base != old.stamp.base {
                        for &id in &old.ids {
                            let mut record = writer.record(id);
                            let local = local(&self.registry, old, id)?;
                            record.meta[3] = base
                                .checked_add(local)
                                .ok_or(Error::Invalid("发光属性槽溢出"))?;
                            writer.replace(id, record)?;
                        }
                    }
                    entries.push(PreparedRegion::Reused { index, stamp });
                    continue;
                }
            }
            let mut ids = reserved(cached.map_or(0, <[Record]>::len))?;
            let old_region = old.map(|index| &self.regions[index]);
            let mut cursor = 0;
            for_each_light(*region, emitters, possible, cached, |mut record| {
                ids.try_reserve(1).map_err(prime_scene::Error::Allocation)?;
                let local_index = record.meta[3];
                record.meta[3] = base
                    .checked_add(local_index)
                    .ok_or(Error::Invalid("发光属性槽溢出"))?;
                if self.regions.is_empty() {
                    ids.push(writer.add(record)?);
                    return Ok(());
                }
                while let Some(old) = old_region
                    && let Some(&id) = old.ids.get(cursor)
                    && local(&self.registry, old, id)? < local_index
                {
                    writer.remove(id)?;
                    cursor += 1;
                }
                let reused = old_region.and_then(|old| old.ids.get(cursor).copied());
                if let Some(id) = reused
                    && local(&self.registry, old_region.expect("已有旧身份"), id)? == local_index
                {
                    writer.replace(id, record)?;
                    ids.push(id);
                    cursor += 1;
                } else {
                    additions
                        .try_reserve(1)
                        .map_err(prime_scene::Error::Allocation)?;
                    additions.push(Addition {
                        entry: entries.len(),
                        position: ids.len(),
                        record,
                    });
                    ids.push(u32::MAX);
                }
                Ok(())
            })?;
            if let Some(old) = old_region {
                for &id in &old.ids[cursor..] {
                    writer.remove(id)?;
                }
            }
            entries.push(PreparedRegion::Built(RegionLights { stamp, ids }));
        }
        for old in &self.regions[old_index..] {
            remove_region(&mut writer, old)?;
        }
        // 所有删除先于新增分配，回收槽可在同一事务内复用，不产生临时容量峰值。
        for addition in additions {
            let id = writer.add(addition.record)?;
            let PreparedRegion::Built(region) = &mut entries[addition.entry] else {
                unreachable!("新增灯属于新region记录")
            };
            region.ids[addition.position] = id;
        }
        let (registry, tables, patches, device) = if device {
            writer.finish_for_gpu()?
        } else {
            let (r, t, p) = writer.finish()?;
            (r, t, p, None)
        };
        Ok(Preparation {
            update: LightingUpdate {
                owner: self.owner,
                version: self.version,
                previous_epoch: self.epoch,
                epoch,
                possible,
                entries,
                next,
                registry,
            },
            tables,
            patches,
            device,
        })
    }

    /// 校验事务归属和世代并预留CPU提交容量；失败或丢弃保留旧目录。
    pub fn publication(&mut self, prepared: Preparation) -> Result<LightingPublication<'_>, Error> {
        let update = &prepared.update;
        if update.owner != self.owner
            || update.version != self.version
            || update.previous_epoch != self.epoch
        {
            return Err(Error::Invalid("发光更新不属于当前所有者或目录版本"));
        }
        let version = self
            .version
            .checked_add(1)
            .ok_or(Error::Invalid("发光目录版本耗尽"))?;
        self.registry
            .reserve(&update.registry, prepared.tables.as_ref())?;
        Ok(LightingPublication {
            lighting: self,
            prepared,
            version,
        })
    }

    /// 稀疏颜色更新的完整输入校验；只查D个有序key，成本O(D log R)，不访问quad。
    pub fn prepare_attribute_ack(
        &mut self,
        changes: &[RenderChange<'_>],
    ) -> Result<AttributeAck<'_>, Error> {
        let mut revisions = reserved(changes.len())?;
        let mut previous = None;
        for change in changes {
            let RenderChange::Upsert(region) = change else {
                return Err(Error::Invalid("颜色确认不能移除发光region"));
            };
            if previous.is_some_and(|key| key >= region.key) || !region.mesh.attribute_only() {
                return Err(Error::Invalid("颜色确认须为唯一有序的属性覆盖"));
            }
            previous = Some(region.key);
            let index = self
                .regions
                .binary_search_by_key(&region.key, |r| r.stamp.key)
                .map_err(|_| Error::Invalid("颜色确认缺少已发布发光region"))?;
            let old = self.regions[index].stamp;
            if region.geometry_revision != old.geometry
                || region.mesh.len() != old.quads
                || region.content_revision < old.content
            {
                return Err(Error::Invalid("颜色确认的几何或内容版本无效"));
            }
            revisions.push((index, region.content_revision));
        }
        let version = if revisions.is_empty() {
            self.version
        } else {
            self.version
                .checked_add(1)
                .ok_or(Error::Invalid("发光目录版本耗尽"))?
        };
        Ok(AttributeAck {
            lighting: self,
            revisions,
            version,
        })
    }

    pub fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }
}

fn remove_region(writer: &mut Writer<'_>, region: &RegionLights) -> Result<(), Error> {
    for &id in &region.ids {
        writer.remove(id)?;
    }
    Ok(())
}

fn local(registry: &Registry, region: &RegionLights, id: u32) -> Result<u32, Error> {
    registry.record(id).meta[3]
        .checked_sub(region.stamp.base)
        .ok_or(Error::Invalid("发光局部身份超出region属性范围"))
}

fn for_each_light(
    region: RenderRegion<'_>,
    emitters: Emitters<'_>,
    possible: bool,
    cached: Option<&[Record]>,
    mut output: impl FnMut(Record) -> Result<(), Error>,
) -> Result<(), Error> {
    if !possible {
        return Ok(());
    }
    if let Some(rows) = cached {
        return rows.iter().copied().try_for_each(output);
    }
    for (index, quad) in region.mesh.quads().enumerate() {
        if let Some(record) = record_for(quad, region.key, index, emitters)? {
            output(record)?;
        }
    }
    Ok(())
}

pub(super) fn record_for(
    quad: &prime_scene::Quad,
    key: [i32; 3],
    local: usize,
    emitters: Emitters<'_>,
) -> Result<Option<Record>, Error> {
    let material = quad.material_id().index() as usize;
    let &sides = emitters
        .sides
        .get(material)
        .ok_or(Error::Invalid("quad材质索引不属于当前资源世代"))?;
    if sides == 0 {
        return Ok(None);
    }
    if sides > 3 {
        return Err(Error::Invalid("发光侧掩码须属于1..=3"));
    }
    let positions = quad.positions();
    if !positions.iter().flatten().all(|v| v.is_finite()) {
        return Err(Error::Invalid("发光quad位置不是有限值"));
    }
    let mut meta = *emitters
        .areas
        .get(material)
        .ok_or(Error::Invalid("quad静态面积身份不属于当前资源世代"))?;
    if meta[0] == u32::MAX && degenerate(positions) {
        return Ok(None);
    }
    meta[3] = u32::try_from(local).map_err(|_| Error::Invalid("发光quad索引溢出"))?;
    let area = light_sampling::QuadArea::new(&positions)?;
    let weight = *emitters
        .weights
        .get(material)
        .ok_or(Error::Invalid("发光亮度不属于当前资源世代"))?;
    let power = light_sampling::emitter_power(area.area(), weight, sides)?;
    let record = Record {
        power,
        meta,
        vertices: light_sampling::pack_vertices(positions),
        region: [key[0] as u32, key[1] as u32, key[2] as u32, sides],
    };
    Ok(Some(record))
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

#[cfg(test)]
mod tests {
    use super::*;
    use prime_scene::{FragmentMask, MaterialId, Mesh, MeshParts, PreparedParts, Quad, TintPatch};

    const AREA: [u32; 4] = [u32::MAX, 0, 0, 0];
    const EMITTERS: Emitters<'static> = Emitters {
        sides: &[3],
        areas: &[AREA],
        weights: &[1.],
    };

    fn mesh(count: usize) -> Mesh {
        Mesh {
            quads: (0..count)
                .map(|i| {
                    let x = i as f32;
                    Quad::with_material(
                        [[x, 0., 0.], [x + 1., 0., 0.], [x + 1., 1., 0.], [x, 1., 0.]],
                        [[0., 0.]; 4],
                        [1.; 3],
                        MaterialId::new(0),
                    )
                })
                .collect(),
        }
    }

    fn view<'a>(regions: &'a [RenderRegion<'a>]) -> RenderView<'a> {
        RenderView {
            generation: 1,
            regions,
            anchor: [0.; 3],
        }
    }

    fn empty(patches: &TablePatches) -> bool {
        patches.selection.is_empty()
            && patches.meta.is_empty()
            && patches.vertices.is_empty()
            && patches.inverse.is_empty()
            && patches.regions.is_empty()
    }

    #[test]
    fn changing_one_emitter_does_not_rebuild_or_upload_the_other_light_records() {
        let mut lighting = Lighting::new().unwrap();
        let mesh = mesh(1_000);
        let regions = [RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }];
        let initial = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        let tree_base = initial.tables.as_ref().unwrap().selection[0][1] as usize;
        lighting.publication(initial).unwrap().commit();
        let ids = lighting.regions[0].ids.clone();
        let mut changed = Mesh {
            quads: mesh.quads.clone(),
        };
        let quad = changed.quads[19];
        changed.quads[19] = Quad::with_material(
            quad.positions().map(|p| [p[0], p[1], p[2] + 1.]),
            quad.uv(),
            quad.tint(),
            quad.material_id(),
        );
        let regions = [RenderRegion {
            key: [0; 3],
            geometry_revision: 2,
            content_revision: 2,
            mesh: (&changed).into(),
        }];
        let prep = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        assert!(prep.tables.is_none());
        assert_eq!(prep.patches.vertices.len(), 3);
        assert!(prep.patches.meta.is_empty());
        assert!(
            prep.patches
                .selection
                .iter()
                .all(|(row, _)| *row == 0 || *row >= tree_base)
        );
        assert!(
            prep.patches
                .selection
                .iter()
                .any(|(row, _)| *row >= tree_base)
        );
        assert!(
            prep.patches
                .vertices
                .iter()
                .all(|(row, _)| *row / 3 == ids[19] as usize)
        );
        assert!(prep.patches.inverse.is_empty());
        assert!(prep.patches.regions.is_empty());
        lighting.publication(prep).unwrap().commit();
        assert_eq!(lighting.regions[0].ids, ids);
    }

    #[test]
    fn resource_brightness_updates_the_tree_without_uploading_geometry() {
        let mut mesh = mesh(2);
        let quad = mesh.quads[1];
        mesh.quads[1] =
            Quad::with_material(quad.positions(), quad.uv(), quad.tint(), MaterialId::new(1));
        let regions = [RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }];
        let mut lighting = Lighting::new().unwrap();
        let emitters = |weights| Emitters {
            sides: &[1, 1],
            areas: &[AREA; 2],
            weights,
        };
        let initial = lighting
            .prepare(view(&regions), &[0], emitters(&[1., 8.]), true, 1)
            .unwrap();
        lighting.publication(initial).unwrap().commit();
        let ids = lighting.regions[0].ids.clone();
        assert_eq!(lighting.registry.record(ids[1]).power, 8.);
        let changed = lighting
            .prepare(view(&regions), &[0], emitters(&[1., 16.]), true, 2)
            .unwrap();
        assert!(!changed.patches.selection.is_empty());
        assert!(changed.patches.vertices.is_empty());
        assert!(changed.patches.meta.is_empty());
        assert!(changed.patches.regions.is_empty());
        lighting.publication(changed).unwrap().commit();
        assert_eq!(lighting.regions[0].ids, ids);
        assert_eq!(lighting.registry.record(ids[0]).power, 1.);
        assert_eq!(lighting.registry.record(ids[1]).power, 16.);
    }

    #[test]
    fn tint_only_updates_reuse_the_light_identity_allocation_and_only_acknowledge_content() {
        let mut parts = MeshParts::default();
        parts.insert(0, mesh(10_000)).unwrap();
        let mut lighting = Lighting::new().unwrap();
        let regions = [RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: parts.view(),
        }];
        let initial = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        lighting.publication(initial).unwrap().commit();
        let ids = lighting.regions[0].ids.as_ptr();
        let tinted = PreparedParts::prepare(&parts, MeshParts::default(), FragmentMask::empty())
            .unwrap()
            .with_tints(
                &parts,
                vec![TintPatch::new(0, 0, 10_000, 0xff10_2030).unwrap()],
            )
            .unwrap();
        let region = RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 2,
            mesh: tinted.view(&parts),
        };
        let prep = lighting
            .prepare(view(&[region]), &[0], EMITTERS, true, 1)
            .unwrap();
        assert!(prep.tables.is_none());
        assert!(empty(&prep.patches));
        lighting.publication(prep).unwrap().commit();
        assert_eq!(lighting.regions[0].ids.as_ptr(), ids);
        let mut region = region;
        region.content_revision = 3;
        lighting
            .prepare_attribute_ack(&[RenderChange::Upsert(region)])
            .unwrap()
            .commit();
        assert_eq!(lighting.regions[0].ids.as_ptr(), ids);
        assert_eq!(lighting.regions[0].stamp.content, 3);
    }

    #[test]
    fn attribute_acknowledgement_validates_the_whole_batch_before_modifying_stamps() {
        let mut parts = MeshParts::default();
        parts.insert(0, mesh(1)).unwrap();
        let tinted = PreparedParts::prepare(&parts, MeshParts::default(), FragmentMask::empty())
            .unwrap()
            .with_tints(&parts, vec![TintPatch::new(0, 0, 1, 0xff00_0000).unwrap()])
            .unwrap();
        let initial = [0, 1].map(|x| RenderRegion {
            key: [x, 0, 0],
            geometry_revision: 1,
            content_revision: 1,
            mesh: parts.view(),
        });
        let mut lighting = Lighting::new().unwrap();
        let prep = lighting
            .prepare(view(&initial), &[0, 1], EMITTERS, true, 1)
            .unwrap();
        lighting.publication(prep).unwrap().commit();
        let changes = initial.map(|mut r| {
            r.mesh = tinted.view(&parts);
            r.content_revision = 2;
            RenderChange::Upsert(r)
        });
        let mut invalid = changes;
        let RenderChange::Upsert(ref mut second) = invalid[1] else {
            unreachable!()
        };
        second.geometry_revision = 2;
        assert!(lighting.prepare_attribute_ack(&invalid).is_err());
        assert!(lighting.regions.iter().all(|r| r.stamp.content == 1));
        lighting.prepare_attribute_ack(&changes).unwrap().commit();
        assert!(lighting.regions.iter().all(|r| r.stamp.content == 2));
    }

    #[test]
    fn failed_or_dropped_publication_keeps_the_old_directory_and_rejects_stale_updates() {
        let mesh = mesh(1);
        let regions = [RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }];
        let mut lighting = Lighting::new().unwrap();
        let failed = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        drop(lighting.publication(failed).unwrap());
        assert!(lighting.regions.is_empty());
        assert_eq!(lighting.version, 0);
        let first = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        let stale = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        lighting.publication(first).unwrap().commit();
        assert!(lighting.publication(stale).is_err());
        let cross = lighting
            .prepare(view(&regions), &[0], EMITTERS, true, 1)
            .unwrap();
        let mut other = Lighting::new().unwrap();
        assert!(other.publication(cross).is_err());
        assert!(other.regions.is_empty());
        assert_eq!(lighting.regions[0].ids.len(), 1);
    }

    #[test]
    fn nonemitting_region_changes_preserve_empty_light_buffers() {
        let mesh = mesh(1);
        let mut lighting = Lighting::new().unwrap();
        for revision in [1, 2] {
            let regions = [RenderRegion {
                key: [0; 3],
                geometry_revision: revision,
                content_revision: revision,
                mesh: (&mesh).into(),
            }];
            let prep = lighting
                .prepare(
                    view(&regions),
                    &[0],
                    Emitters {
                        sides: &[0],
                        areas: &[AREA],
                        weights: &[1.],
                    },
                    false,
                    1,
                )
                .unwrap();
            assert!(prep.tables.is_none());
            assert!(empty(&prep.patches));
            lighting.publication(prep).unwrap().commit();
        }
    }

    #[test]
    fn material_changes_register_and_remove_emission_without_changing_geometry_revision() {
        let dark = mesh(1);
        let quad = dark.quads[0];
        let lit = Mesh {
            quads: vec![Quad::with_material(
                quad.positions(),
                quad.uv(),
                quad.tint(),
                MaterialId::new(1),
            )],
        };
        let mut lighting = Lighting::new().unwrap();
        for (revision, mesh, count) in [(1, &dark, 0), (2, &lit, 1), (3, &dark, 0), (4, &lit, 1)] {
            let regions = [RenderRegion {
                key: [0; 3],
                geometry_revision: 1,
                content_revision: revision,
                mesh: mesh.into(),
            }];
            let prep = lighting
                .prepare(
                    view(&regions),
                    &[0],
                    Emitters {
                        sides: &[0, 3],
                        areas: &[AREA, AREA],
                        weights: &[1., 1.],
                    },
                    true,
                    1,
                )
                .unwrap();
            lighting.publication(prep).unwrap().commit();
            assert_eq!(lighting.regions[0].ids.len(), count);
            assert_eq!(lighting.regions[0].stamp.geometry, 1);
        }
    }
}
