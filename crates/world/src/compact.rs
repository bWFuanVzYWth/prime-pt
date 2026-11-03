//! 压缩源缓存、显式halo及同步slab/tint阶段；所有数组归world，不持有宿主指针。
use super::*;
use prime_minecraft::{Catalog, Neighbor, Neighborhood, SectionSource, TintRequest};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub(super) struct Sources {
    pub generation: u64,
    pub values: BTreeMap<[i32; 3], SectionSource>,
    pub tints: Vec<PendingTint>,
    pub queries: Vec<TintRequest>,
    pub indices: HashMap<TintRequest, usize>,
}

pub(super) struct PendingTint {
    pub ticket: Ticket,
    pub slot: usize,
    pub patches: Vec<(usize, usize)>,
}

pub(super) fn needed(region: [i32; 3], dirty: u64) -> Vec<[i32; 3]> {
    let mut mask = [0u64; 4];
    let mut output = Vec::with_capacity(216);
    for slot in 0..64 {
        if dirty & (1 << slot) == 0 {
            continue;
        }
        let key = section(region, slot);
        output.push(key);
        let local = [slot / 16, (slot / 4) % 4, slot % 4];
        for x in 0..3 {
            for y in 0..3 {
                for z in 0..3 {
                    let index = (local[0] + x) * 36 + (local[1] + y) * 6 + local[2] + z;
                    mask[index / 64] |= 1 << (index % 64);
                }
            }
        }
    }
    for key in &output {
        let local =
            std::array::from_fn::<_, 3, _>(|axis| (key[axis] - region[axis] * 4 + 1) as usize);
        let index = local[0] * 36 + local[1] * 6 + local[2];
        mask[index / 64] &= !(1 << (index % 64));
    }
    for index in 0..216 {
        if mask[index / 64] & (1 << (index % 64)) != 0 {
            let local = [index / 36, (index / 6) % 6, index % 6];
            output.push(std::array::from_fn(|axis| {
                region[axis] * 4 + local[axis] as i32 - 1
            }));
        }
    }
    output
}

impl Sources {
    pub(super) fn prune_tints(&mut self) {
        if self.tints.is_empty() {
            self.queries.clear();
            self.indices.clear();
            return;
        }
        let mut remap = vec![usize::MAX; self.queries.len()];
        for tint in &self.tints {
            for (_, color) in &tint.patches {
                remap[*color] = 0;
            }
        }
        let mut count = 0;
        for (index, value) in remap.iter_mut().enumerate() {
            if *value != usize::MAX {
                *value = count;
                self.queries[count] = self.queries[index];
                count += 1;
            }
        }
        self.queries.truncate(count);
        self.indices.clear();
        self.indices.extend(
            self.queries
                .iter()
                .enumerate()
                .map(|(index, value)| (*value, index)),
        );
        for tint in &mut self.tints {
            for (_, color) in &mut tint.patches {
                *color = remap[*color];
            }
        }
    }
}

/// 已预留CPU State的资源世代事务；未提交时丢弃即可，原Scene不变。
///
/// 活跃事务独占原所有者，不能跨阶段清场后再提交旧事务：
/// ```compile_fail,E0499
/// fn stale(scene: &mut prime_world::Scene) {
///     let prepared = scene.stage_source_generation(2).unwrap();
///     scene.clear();
///     prepared.commit();
/// }
/// ```
pub struct PreparedGeneration<'a> {
    scene: &'a mut Scene,
    generation: u64,
    state: Option<State<RegionMesh>>,
}

impl PreparedGeneration<'_> {
    /// GPU资源事务成功后仅移动预留状态，不返回错误；保留pool/wire/revision/预算。
    /// 独占借用将stage绑定原Scene，不能交给其他所有者或与较新的stage交错提交。
    pub fn commit(self) {
        if let Some(state) = self.state {
            self.scene.clear_with(state);
        }
        self.scene.compact.generation = self.generation;
    }
}

impl Scene {
    /// 预留世代切换所需的State；错误和分配均位于GPU资源发布之前。
    pub fn stage_source_generation(
        &mut self,
        generation: u64,
    ) -> Result<PreparedGeneration<'_>, Error> {
        if generation == 0 {
            return Err(Error::Input("压缩源资源世代为零"));
        }
        let state = if self.compact.generation == generation {
            None
        } else {
            Some(State::new(configuration(
                initial_bounds(),
                Target::new([0; 3])?,
            )?))
        };
        Ok(PreparedGeneration {
            scene: self,
            generation,
            state,
        })
    }

    /// 无GPU事务的便利入口；生产资源路径使用stage/commit分离。
    pub fn source_generation(&mut self, generation: u64) -> Result<(), Error> {
        let prepared = self.stage_source_generation(generation)?;
        prepared.commit();
        Ok(())
    }

    /// 当前完整资源世代；零表示尚未进入压缩生产输入或兼容测试。
    pub const fn generation(&self) -> u64 {
        self.compact.generation
    }

    /// 消费PRT3自持压缩源；先验证整个响应的票据/坐标/世代，再更新唯一source cache。
    pub fn apply_compact(&mut self, bytes: &[u8], catalog: &Catalog) -> Result<(), Error> {
        let sources = prime_minecraft::decode_sections(bytes)?;
        for source in &sources {
            if self
                .captures
                .iter()
                .any(|capture| capture.wire.id == source.request)
                && let Some(storage) = &source.storage
            {
                catalog.validate_section(storage)?;
            }
            if let Some(capture) = self
                .captures
                .iter()
                .find(|capture| capture.wire.id == source.request)
                && (capture.wire.section != source.key
                    || source.generation != self.compact.generation
                    || source.generation != catalog.generation())
            {
                return Err(Error::Input("压缩源票据、坐标或资源世代不匹配"));
            }
        }
        for source in sources {
            if let Some(index) = self
                .captures
                .iter()
                .position(|capture| capture.wire.id == source.request)
            {
                self.captures.remove(index);
                self.compact.values.insert(source.key, source);
            }
        }
        self.compile_sources(catalog)
    }

    /// 待宿主求值的唯一tint完整键，首次出现顺序；不重复执行同一原版回调。
    pub fn tint_requests(&self) -> impl ExactSizeIterator<Item = TintRequest> + '_ {
        self.compact.queries.iter().copied()
    }

    /// 完整回填上次唯一请求的ARGB；先校验长度，无默认白色退路。
    pub fn apply_tints(&mut self, colors: &[u32]) -> Result<(), Error> {
        if colors.len() != self.compact.queries.len() {
            return Err(Error::Input("world tint响应数量不匹配"));
        }
        let changed = !self.compact.tints.is_empty();
        for pending in self.compact.tints.drain(..) {
            let job = self
                .jobs
                .iter_mut()
                .find(|job| job.ticket == pending.ticket)
                .expect("tint所有者保活");
            let mesh = &mut job
                .sections
                .iter_mut()
                .find(|(slot, _)| *slot == pending.slot)
                .expect("tint阶段保持mesh")
                .1;
            for (quad, color) in pending.patches {
                let value = colors[color];
                mesh.quads[quad]
                    .set_tint([16, 8, 0].map(|shift| ((value >> shift) & 255) as f32 / 255.));
            }
            job.received |= 1 << pending.slot;
        }
        self.compact.queries.clear();
        self.compact.indices.clear();
        self.finish_jobs()?;
        if changed {
            self.planner_pending = true;
        }
        Ok(())
    }

    fn neighborhood<'a>(&'a self, key: [i32; 3], generation: u64) -> Neighborhood<'a> {
        Neighborhood {
            key,
            generation,
            sections: std::array::from_fn(|index| {
                let offset = [index % 3, (index / 9) % 3, (index / 3) % 3].map(|v| v as i32 - 1);
                let position = std::array::from_fn(|axis| key[axis] + offset[axis]);
                if position[1] < self.height[0] || position[1] >= self.height[1] {
                    return Neighbor::Empty;
                }
                match self.compact.values.get(&position) {
                    Some(source) => source
                        .storage
                        .as_ref()
                        .map_or(Neighbor::Unavailable, Neighbor::Available),
                    None => Neighbor::Unknown,
                }
            }),
        }
    }

    /// 在同一私有池同步编译已齐备源；每波两个section×4 slab，无嵌套调度。
    /// 缺失halo延后，已知不可用保留旧region覆盖与负缓存；每份输出受剩余payload限额约束。
    pub fn compile_sources(&mut self, catalog: &Catalog) -> Result<(), Error> {
        if self.compact.generation == 0 {
            return Ok(());
        }
        if catalog.generation() != self.compact.generation {
            return Err(Error::Input("world/Catalog世代不匹配"));
        }
        let before_jobs = self.jobs.len();
        let stride = size_of::<prime_scene::Quad>();
        let limit = (self.budget.resident_bytes + self.budget.staging_bytes) / stride;
        let mut index = 0;
        while index < self.jobs.len() {
            let ticket = self.jobs[index].ticket;
            let mut complete = true;
            let mut unavailable = false;
            for key in &self.jobs[index].needed {
                if key[1] < self.height[0] || key[1] >= self.height[1] {
                    continue;
                }
                match self.compact.values.get(key) {
                    None => complete = false,
                    Some(source) if source.storage.is_none() => unavailable = true,
                    _ => {}
                }
            }
            if unavailable {
                self.jobs[index].unavailable = 1;
                index += 1;
                continue;
            }
            if !complete {
                index += 1;
                continue;
            }
            let slots: Vec<_> = (0..64)
                .filter(|slot| {
                    self.jobs[index].dirty & (1 << slot) != 0
                        && self.jobs[index].received & (1 << slot) == 0
                        && !self.jobs[index].sections.iter().any(|(at, _)| at == slot)
                })
                .collect();
            for wave in slots.chunks(2) {
                let available = limit.saturating_sub(self.retained_quads);
                let work: Vec<_> = wave
                    .iter()
                    .flat_map(|slot| [0, 4, 8, 12].map(move |first_y| (*slot, first_y)))
                    .collect();
                let quota = available / work.len();
                let region = self.jobs[index].region;
                let mut output: Vec<_> = self.pool.install(|| {
                    work.par_iter()
                        .map(|(slot, first_y)| {
                            let key = section(region, *slot);
                            prime_minecraft::compile_section_slab_limited(
                                catalog,
                                &self.neighborhood(key, catalog.generation()),
                                *first_y,
                                quota,
                            )
                        })
                        .collect()
                });
                let mut used: usize = output
                    .iter()
                    .filter_map(|entry| entry.as_ref().ok())
                    .map(|entry| entry.mesh.quads.len())
                    .sum();
                let mut exceeded = None;
                for (at, result) in output.iter_mut().enumerate() {
                    if matches!(result, Err(prime_minecraft::Error::OutputLimit { .. })) {
                        let (slot, first_y) = work[at];
                        *result = prime_minecraft::compile_section_slab_limited(
                            catalog,
                            &self.neighborhood(section(region, slot), catalog.generation()),
                            first_y,
                            available - used,
                        );
                        if let Ok(value) = result {
                            used += value.mesh.quads.len();
                        }
                    }
                    match result {
                        Err(prime_minecraft::Error::OutputLimit { required }) => {
                            exceeded = Some(*required);
                            break;
                        }
                        Err(error) => {
                            return Err(Error::Minecraft(std::mem::replace(
                                error,
                                prime_minecraft::Error::Input("已移出的编译错误"),
                            )));
                        }
                        _ => {}
                    }
                }
                if let Some(required) = exceeded {
                    if let Some(victim) = self
                        .jobs
                        .iter()
                        .rposition(|job| job.ticket != ticket && !job.sections.is_empty())
                    {
                        let job = self.drop_job(victim);
                        let _ = self.state.fail(job.ticket);
                        self.finish_jobs()?;
                        self.planner_pending = true;
                        return Ok(());
                    }
                    let bounds = self.state.config().bounds().expect("有界目标");
                    let outgoing = self.state.visible().any(|(_, chunk, region)| {
                        !bounds.intersects(chunk) && region.owned_quads() != 0
                    });
                    let region_required =
                        (self.estimate_job(&self.jobs[index], &[]) + used + required) * stride;
                    if outgoing
                        || self.state.retired_len() != 0
                        || !self.compact.tints.is_empty()
                        || region_required <= self.budget.region_bytes
                    {
                        self.blocked_quads = used + required;
                        self.pressure = Pressure {
                            kind: PressureKind::Transition,
                            required_bytes: (self.retained_quads + self.blocked_quads) * stride,
                            limit_bytes: limit * stride,
                            ..Pressure::default()
                        };
                        self.finish_jobs()?;
                        self.planner_pending = true;
                        return Ok(());
                    }
                    self.exclude_job(
                        index,
                        PressureKind::Region,
                        region_required,
                        self.budget.region_bytes,
                    )?;
                    break;
                }
                let mut outputs = output.into_iter();
                for slot in wave {
                    let parts: Vec<_> = outputs
                        .by_ref()
                        .take(4)
                        .map(|entry| entry.expect("上方已验证slab结果"))
                        .collect();
                    let mut mesh = Mesh::default();
                    let mut patches = Vec::new();
                    for mut part in parts {
                        let base = mesh.quads.len();
                        if mesh.quads.is_empty() {
                            mesh.quads = std::mem::take(&mut part.mesh.quads);
                        } else {
                            mesh.quads.append(&mut part.mesh.quads);
                        }
                        patches.extend(part.tints.into_iter().map(|mut patch| {
                            patch.quad += base;
                            patch
                        }));
                    }
                    let offset = section(region, *slot).map(|v| v.rem_euclid(4) as f32 * 16.);
                    if offset != [0.; 3] {
                        for quad in &mut mesh.quads {
                            quad.translate(offset);
                        }
                    }
                    self.retained_quads += mesh.quads.len();
                    let job = self
                        .jobs
                        .iter_mut()
                        .find(|job| job.ticket == ticket)
                        .expect("同步内核期间保持job");
                    job.sections.push((*slot, mesh));
                    if patches.is_empty() {
                        job.received |= 1 << slot;
                    } else {
                        let aliases = patches
                            .into_iter()
                            .map(|patch| {
                                let color =
                                    if let Some(index) = self.compact.indices.get(&patch.request) {
                                        *index
                                    } else {
                                        let index = self.compact.queries.len();
                                        self.compact.queries.push(patch.request);
                                        self.compact.indices.insert(patch.request, index);
                                        index
                                    };
                                (patch.quad, color)
                            })
                            .collect();
                        self.compact.tints.push(PendingTint {
                            ticket,
                            slot: *slot,
                            patches: aliases,
                        });
                    }
                }
                let mut no_sources = Vec::new();
                self.admit_sources(&mut no_sources)?;
                if !self.jobs.iter().any(|job| job.ticket == ticket) {
                    break;
                }
            }
            if self.jobs.get(index).is_some_and(|job| job.ticket == ticket) {
                index += 1;
            }
        }
        self.finish_jobs()?;
        if self.jobs.len() != before_jobs {
            self.planner_pending = true;
        }
        Ok(())
    }
}
