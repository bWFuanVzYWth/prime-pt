//! 以64m region为唯一LOD0资源；一个资源合并4³个16m section，GPU据此构建一个BLAS。
//!
//! 水平需求按Minecraft视距（section列数）取相机列两侧的闭区间，再向外对齐region；
//! Y覆盖宿主给出的完整半开section高度域，域外位置由该边界证明为空，不请求宿主。
//! 引擎球包住整个有限Bounds，只承担规划优先级，RootCursor先按各轴裁剪Bounds。
//! State独占已完成的合并Mesh、ticket与退役资源；job仅持有本轮dirty section，
//! 完成后按slot顺序将旧资源未修改范围与新范围合并，释放局部阶段缓冲。
//!
//! 交集资源和ticket保留。最近已解析Bounds的旧覆盖保留到新域全部region解析完；
//! Unavailable是已解析的负缓存，不是空section或真实可用覆盖；首次Unavailable即可
//! 完成负缓存并撤掉整个旧region，恢复加载通知后全64重新捕获才可恢复几何。
//! 过期中间前沿可预算淘汰，防止连续瞬移耗尽槽。目标域两倍加64槽必须不超过8192，
//! 超域返回错误，不缩小视距或清场。当前、替换、退役、partial及合并临时Mesh的联合
//! 工作集均受16M顶点限制；目标域单独可容纳不保证过渡联合几何也可容纳。
//!
//! 每次plan最多1024个引擎步骤、8个在途region job、64条在途section捕获；调用者
//! 单独限制待捕获返回数（至多64，宿主按时间预算捕获前缀）。另各推进64项失效/退役。prepare扫描8192槽、
//! 排序非空Handle视图，以及编译/合并工作不计入cm::Budget。未请求/未回复不当作空。

use super::{MAX_VERTICES, Mesh, Section, WorldCamera, compile};
use crate::Error;
use chunk_manager::{self as cm, Bounds, Change, Chunk, Handle, Patch, State, Target, Ticket};
use rayon::prelude::*;

const CAPACITY: usize = 8192;
const REQUESTS: usize = 64;
const JOBS: usize = 8;
const SWITCHES: usize = 64;
const VISITS: usize = 1024;
const SCAN: usize = 64;

/// 每条24字节；id在会话中单调，世界clear后也不复用。坐标仍以16m section计。
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct CaptureRequest {
    pub id: u64,
    pub section: [i32; 3],
    pub reserved: u32,
}
const _: () = assert!(size_of::<CaptureRequest>() == 24);

struct PendingCapture {
    wire: CaptureRequest,
    ticket: Ticket,
    slot: usize,
}

struct RegionMesh {
    mesh: Mesh,
    // slot = x * 16 + y * 4 + z；每个范围的quad位置均为region local。
    ranges: [usize; 65],
    revision: u64,
    available: bool,
}

struct RegionJob {
    ticket: Ticket,
    handle: Handle,
    region: [i32; 3],
    dirty: u64,
    issued: u64,
    received: u64,
    unavailable: u64,
    sections: Vec<(usize, Mesh)>,
}

#[derive(Clone, Copy, Default)]
struct Dirty {
    handle: Option<Handle>,
    mask: u64,
}

struct View {
    handle: Handle,
    prepared: bool,
}

pub(crate) struct Scene {
    state: State<RegionMesh>,
    batch: cm::Plan,
    pending: Vec<Patch>,
    removes: Vec<Patch>,
    view: Vec<View>,
    jobs: Vec<RegionJob>,
    masks: Vec<Dirty>,
    captures: Vec<PendingCapture>,
    output: Vec<CaptureRequest>,
    pool: rayon::ThreadPool,
    target: Option<Target>,
    resolved: Option<Bounds>,
    next_resolved: Option<Bounds>,
    height: [i32; 2],
    anchor: [f64; 3],
    next_wire: u64,
    next_revision: u64,
    retained_quads: usize,
    scan: Option<usize>,
    scan_again: bool,
    planner_pending: bool,
    dirty: bool,
}

fn volume(bounds: Bounds) -> usize {
    bounds
        .minimum()
        .into_iter()
        .zip(bounds.maximum())
        .map(|(lo, hi)| (hi - lo) as usize)
        .product()
}

fn configuration(bounds: Bounds, target: Target) -> Result<cm::Config, cm::ConfigError> {
    // L1上界包住全部region AABB；不改变引擎radius>=4、root radius+2契约。
    let radius: u64 = (0..3)
        .map(|i| {
            let lo = bounds.minimum()[i];
            let hi = bounds.maximum()[i];
            target.coords()[i]
                .abs_diff(lo)
                .max(target.coords()[i].abs_diff(hi))
        })
        .sum();
    Ok(cm::Config::new(
        1,
        u32::try_from(radius.max(4)).map_err(|_| cm::ConfigError::Radius)?,
        CAPACITY,
    )?
    .with_bounds(Some(bounds)))
}

fn initial_bounds() -> Bounds {
    Bounds::new([0; 3], [1; 3]).expect("初始非空region占位域")
}

fn chunk(key: [i32; 3]) -> Chunk {
    Chunk::new(key.map(i64::from), 0)
}
fn region(section: [i32; 3]) -> [i32; 3] {
    section.map(|v| v.div_euclid(4))
}
fn slot(section: [i32; 3]) -> usize {
    let p = section.map(|v| v.rem_euclid(4) as usize);
    p[0] * 16 + p[1] * 4 + p[2]
}
fn section(region: [i32; 3], slot: usize) -> [i32; 3] {
    let local = [slot / 16, (slot / 4) % 4, slot % 4];
    std::array::from_fn(|i| region[i] * 4 + local[i] as i32)
}
fn key(value: Chunk) -> [i32; 3] {
    value
        .coord
        .map(|v| i32::try_from(v).expect("输入域保持region坐标在i32内"))
}

impl Scene {
    pub(crate) fn new() -> Result<Self, Error> {
        let threads = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(8);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("prt-mesh-{i}"))
            .build()?;
        Ok(Self {
            state: State::new(configuration(initial_bounds(), Target::new([0; 3])?)?),
            batch: cm::Plan::with_capacity(JOBS, SWITCHES),
            pending: Vec::with_capacity(SWITCHES),
            removes: Vec::with_capacity(CAPACITY),
            view: Vec::with_capacity(CAPACITY),
            jobs: Vec::with_capacity(JOBS),
            masks: vec![Dirty::default(); CAPACITY],
            captures: Vec::with_capacity(REQUESTS),
            output: Vec::with_capacity(REQUESTS),
            pool,
            target: None,
            resolved: None,
            next_resolved: None,
            height: [0, 4],
            anchor: [0.; 3],
            next_wire: 1,
            next_revision: 1,
            retained_quads: 0,
            scan: None,
            scan_again: false,
            planner_pending: true,
            dirty: true,
        })
    }

    /// 仅世界身份域替换清场；wire计数与几何世代不归零。
    pub(crate) fn clear(&mut self) {
        self.state = State::new(
            configuration(initial_bounds(), Target::new([0; 3]).expect("零目标"))
                .expect("初始固定配置"),
        );
        self.batch.clear();
        self.pending.clear();
        self.removes.clear();
        self.view.clear();
        self.jobs.clear();
        self.masks.fill(Dirty::default());
        self.captures.clear();
        self.output.clear();
        self.target = None;
        self.resolved = None;
        self.next_resolved = None;
        self.retained_quads = 0;
        self.scan = None;
        self.scan_again = false;
        self.planner_pending = true;
        self.dirty = true;
    }

    pub(crate) fn rebase(&mut self, anchor: [f64; 3]) {
        if self.anchor != anchor {
            self.anchor = anchor;
            self.dirty = true;
        }
    }
    pub(crate) const fn anchor(&self) -> [f64; 3] {
        self.anchor
    }

    fn select(
        &mut self,
        camera: &WorldCamera,
        distance: u32,
        min_y: i32,
        max_y: i32,
    ) -> Result<Target, Error> {
        let anchor = camera.anchor()?;
        if min_y >= max_y || min_y.checked_mul(16).is_none() || max_y.checked_mul(16).is_none() {
            return Err(Error::Input("世界section高度域无效"));
        }
        let center = camera.origin.map(|v| (v / 16.0).floor() as i64);
        let d = i64::from(distance);
        let minimum = [
            (center[0] - d).div_euclid(4),
            i64::from(min_y).div_euclid(4),
            (center[2] - d).div_euclid(4),
        ];
        let maximum = [
            (center[0] + d).div_euclid(4) + 1,
            (i64::from(max_y) - 1).div_euclid(4) + 1,
            (center[2] + d).div_euclid(4) + 1,
        ];
        let mut count = 1usize;
        for i in 0..3 {
            if minimum[i] < i64::from(i32::MIN / 64) || maximum[i] > i64::from(i32::MAX / 64) {
                return Err(Error::Input("视距region坐标超过源协议域"));
            }
            count = count
                .checked_mul((maximum[i] - minimum[i]) as usize)
                .ok_or(Error::Input("视距region数量溢出"))?;
        }
        if count
            .checked_mul(2)
            .and_then(|n| n.checked_add(SWITCHES))
            .is_none_or(|n| n > CAPACITY)
        {
            return Err(Error::Input("视距与世界高度的过渡region数量超过8192槽容量"));
        }
        let bounds = Bounds::new(minimum, maximum)?;
        let target = Target::new(camera.origin.map(|v| (v / 64.0).floor() as i64))?;
        self.state.reconfigure(configuration(bounds, target)?)?;
        if self.target.is_some() && self.height != [min_y, max_y] {
            // job的域外空证明绑定旧高度域；新域不能消费这些阶段结果。
            let handles: Vec<_> = self.jobs.iter().map(|job| job.handle).collect();
            for handle in handles {
                self.invalidate_handle(handle, u64::MAX)?;
            }
            self.invalidate_all();
        }
        self.height = [min_y, max_y];
        self.target = Some(target);
        self.rebase(anchor);
        Ok(target)
    }

    fn mark_dirty(&mut self, handle: Handle, mask: u64) {
        let value = &mut self.masks[handle.slot()];
        if value.handle != Some(handle) {
            *value = Dirty {
                handle: Some(handle),
                mask: 0,
            };
        }
        value.mask |= mask;
    }

    fn drop_job(&mut self, index: usize) -> RegionJob {
        let job = self.jobs.remove(index);
        self.captures.retain(|capture| capture.ticket != job.ticket);
        self.retained_quads -= job
            .sections
            .iter()
            .map(|(_, mesh)| mesh.quads.len())
            .sum::<usize>();
        self.mark_dirty(job.handle, job.dirty);
        job
    }

    fn invalidate_handle(&mut self, handle: Handle, mask: u64) -> Result<(), Error> {
        self.mark_dirty(handle, mask);
        if let Some(index) = self.jobs.iter().position(|job| job.handle == handle) {
            let job = self.drop_job(index);
            match self.state.fail(job.ticket) {
                Ok(()) | Err(cm::StateError::StaleTicket) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.state.invalidate(handle)?;
        self.planner_pending = true;
        Ok(())
    }

    pub(crate) fn invalidate_section(&mut self, value: [i32; 3]) -> Result<(), Error> {
        if let Some(handle) = self.state.find(chunk(region(value))) {
            self.invalidate_handle(handle, 1 << slot(value))?;
        }
        Ok(())
    }

    /// 分步扫描；重复通知至多保留一轮额外回扫，不重置已推进游标。
    pub(crate) fn invalidate_all(&mut self) {
        if self.scan.is_some() {
            self.scan_again = true;
        } else {
            self.scan = Some(0);
        }
        self.planner_pending = true;
    }
    fn scan_invalidations(&mut self) -> Result<(), Error> {
        for _ in 0..SCAN {
            let Some(index) = self.scan else {
                break;
            };
            if let Some(handle) = self.state.handle_at(index) {
                self.invalidate_handle(handle, u64::MAX)?;
            }
            self.scan = if index + 1 == CAPACITY {
                if std::mem::take(&mut self.scan_again) {
                    Some(0)
                } else {
                    None
                }
            } else {
                Some(index + 1)
            };
        }
        Ok(())
    }
    fn drain_retired(&mut self) {
        for _ in 0..SCAN {
            let Some(resource) = self.state.take_retired(0) else {
                break;
            };
            self.retained_quads -= resource.mesh.quads.len();
        }
    }

    /// 视距单位为16m水平列；Y为半开世界section域。返回仍待宿主捕获的请求前缀。
    ///
    /// 分帧回复共享同一region ticket，完整64位置（含高度域证明为空的位置）才完成资源。
    /// 输入超域保留旧状态；max_requests最多64且同时在途最多64，未完成请求可延后回复。
    pub(crate) fn plan(
        &mut self,
        camera: &WorldCamera,
        view_distance: u32,
        min_section_y: i32,
        max_section_y_exclusive: i32,
        max_requests: usize,
    ) -> Result<&[CaptureRequest], Error> {
        let target = self.select(
            camera,
            view_distance,
            min_section_y,
            max_section_y_exclusive,
        )?;
        self.reject()?;
        self.output.clear();
        self.drain_retired();
        self.scan_invalidations()?;
        let result = cm::plan(
            &mut self.state,
            target,
            cm::Budget {
                visits: VISITS,
                requests: JOBS - self.jobs.len(),
                switches: SWITCHES,
            },
            &mut self.batch,
        );
        for at in 0..self.batch.cancellations().len() {
            let ticket = self.batch.cancellations()[at];
            if let Some(index) = self.jobs.iter().position(|job| job.ticket == ticket) {
                self.drop_job(index);
            }
        }
        for build in self.batch.requests() {
            let handle = self.state.find(build.chunk).expect("已签发region保持节点");
            let mask = self.masks[handle.slot()];
            let dirty = if self
                .state
                .resource(handle)
                .is_some_and(|resource| resource.available)
                && mask.handle == Some(handle)
                && mask.mask != 0
            {
                mask.mask
            } else {
                u64::MAX
            };
            self.masks[handle.slot()] = Dirty {
                handle: Some(handle),
                mask: 0,
            };
            let mut job = RegionJob {
                ticket: build.ticket,
                handle,
                region: key(build.chunk),
                dirty,
                issued: 0,
                received: 0,
                unavailable: 0,
                sections: Vec::new(),
            };
            for slot in 0..64 {
                let y = section(job.region, slot)[1];
                if y < self.height[0] || y >= self.height[1] {
                    job.received |= (1 << slot) & dirty;
                }
            }
            self.jobs.push(job);
        }
        for patch in self.batch.switches() {
            if matches!(patch.change, Change::Remove(_)) {
                self.removes.push(*patch);
            } else {
                self.pending.push(*patch);
            }
        }
        self.batch.clear();
        self.planner_pending = result?.pending;
        let count = max_requests.min(REQUESTS);
        let additional = count.saturating_sub(self.captures.len());
        self.next_wire
            .checked_add(additional as u64)
            .ok_or(Error::Input("捕获请求编号溢出"))?;
        let bounds = self.state.config().bounds().expect("有界region域");
        for job in &mut self.jobs {
            if !bounds.intersects(chunk(job.region)) {
                continue;
            }
            let mut available = job.dirty & !(job.issued | job.received);
            while available != 0 && self.captures.len() < count {
                let slot = available.trailing_zeros() as usize;
                let bit = 1 << slot;
                available &= !bit;
                job.issued |= bit;
                let wire = CaptureRequest {
                    id: self.next_wire,
                    section: section(job.region, slot),
                    reserved: 0,
                };
                self.next_wire += 1;
                self.captures.push(PendingCapture {
                    wire,
                    ticket: job.ticket,
                    slot,
                });
            }
        }
        self.output
            .extend(self.captures.iter().take(count).map(|capture| capture.wire));
        self.finish_jobs()?;
        Ok(&self.output)
    }

    pub(crate) fn fail_capture(&mut self, id: u64) -> Result<(), Error> {
        if let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.wire.id == id)
        {
            let capture = self.captures.remove(index);
            if let Some(job) = self
                .jobs
                .iter_mut()
                .find(|job| job.ticket == capture.ticket)
            {
                job.issued &= !(1 << capture.slot);
            }
            self.planner_pending = true;
        }
        Ok(())
    }

    pub(crate) fn apply(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let sources = super::decode(bytes)?;
        #[cfg(test)]
        if sources.first().is_some_and(|source| source.id == 0) {
            return self.apply_legacy(&sources);
        }
        self.complete_sources(&sources)
    }

    fn complete_sources(&mut self, sources: &[Section<'_>]) -> Result<(), Error> {
        let bounds = self.state.config().bounds().expect("有界region域");
        let mut selected = Vec::with_capacity(sources.len());
        for source in sources {
            if let Some(capture) = self
                .captures
                .iter()
                .find(|capture| capture.wire.id == source.id)
            {
                if capture.wire.section != source.key {
                    return Err(Error::Input("捕获id与section坐标不匹配"));
                }
                if bounds.intersects(chunk(region(source.key))) {
                    selected.push((source, capture.ticket, capture.slot));
                }
            }
        }
        let unavailable: Vec<_> = selected
            .iter()
            .filter(|(source, _, _)| !source.available)
            .map(|(_, ticket, _)| *ticket)
            .collect();
        let incoming = selected
            .iter()
            .filter(|(_, ticket, _)| !unavailable.contains(ticket))
            .map(|(source, _, _)| source.quads)
            .sum();
        self.check_quads(incoming)?;
        let build = |(source, ticket, _): &(&Section<'_>, Ticket, usize)| {
            if unavailable.contains(ticket) {
                return Mesh::default();
            }
            let mut mesh = compile(source);
            let offset = source.key.map(|v| v.rem_euclid(4) as f32 * 16.0);
            if offset != [0.; 3] {
                for quad in &mut mesh.quads {
                    quad.translate(offset);
                }
            }
            mesh
        };
        let meshes: Vec<_> = if selected.len() < 4 || incoming < 1024 {
            selected.iter().map(build).collect()
        } else {
            self.pool
                .install(|| selected.par_iter().map(build).collect())
        };
        for ((source, ticket, slot), mesh) in selected.into_iter().zip(meshes) {
            if let Some(job) = self.jobs.iter_mut().find(|job| job.ticket == ticket) {
                self.retained_quads += mesh.quads.len();
                job.sections.push((slot, mesh));
                job.received |= 1 << slot;
                if !source.available {
                    job.unavailable |= 1 << slot;
                }
            }
            self.captures.retain(|capture| capture.wire.id != source.id);
        }
        for source in sources {
            if !bounds.intersects(chunk(region(source.key))) {
                self.fail_capture(source.id)?;
            }
        }
        self.finish_jobs()?;
        self.planner_pending = true;
        Ok(())
    }

    fn check_quads(&self, additional: usize) -> Result<(), Error> {
        if self
            .retained_quads
            .checked_add(additional)
            .is_none_or(|n| n > MAX_VERTICES / 4)
        {
            Err(Error::Input("region过渡联合Mesh超过16M顶点预算"))
        } else {
            Ok(())
        }
    }

    fn finish_jobs(&mut self) -> Result<(), Error> {
        // 一批候选先全部检查/合并，所有fallible边界通过后才交给State，避免发布包前缀。
        let mut layouts = Vec::with_capacity(self.jobs.len());
        let mut incoming = 0usize;
        for job in self
            .jobs
            .iter()
            .filter(|job| job.unavailable != 0 || job.received == job.dirty)
        {
            let old = self.state.resource(job.handle);
            let mut ranges = [0usize; 65];
            for slot in 0..64 {
                let len = if job.unavailable != 0 {
                    0
                } else if job.dirty & (1 << slot) != 0 {
                    job.sections
                        .iter()
                        .find(|(at, _)| *at == slot)
                        .map_or(0, |(_, mesh)| mesh.quads.len())
                } else {
                    old.map_or(0, |old| old.ranges[slot + 1] - old.ranges[slot])
                };
                ranges[slot + 1] = ranges[slot] + len;
            }
            incoming = incoming
                .checked_add(ranges[64])
                .ok_or(Error::Input("region几何数量溢出"))?;
            layouts.push((job, ranges));
        }
        self.check_quads(incoming)?;
        let next = self
            .next_revision
            .checked_add(layouts.len() as u64)
            .ok_or(Error::Input("region revision溢出"))?;
        let mut candidates = Vec::with_capacity(layouts.len());
        for (at, (job, ranges)) in layouts.into_iter().enumerate() {
            let old = self.state.resource(job.handle);
            let mut mesh = Mesh {
                quads: Vec::with_capacity(ranges[64]),
            };
            for slot in 0..64 {
                if job.unavailable != 0 {
                    break;
                }
                if job.dirty & (1 << slot) != 0 {
                    if let Some((_, part)) = job.sections.iter().find(|(at, _)| *at == slot) {
                        mesh.quads.extend_from_slice(&part.quads);
                    }
                } else if let Some(old) = old {
                    mesh.quads
                        .extend_from_slice(&old.mesh.quads[old.ranges[slot]..old.ranges[slot + 1]]);
                }
            }
            let revision = old
                .filter(|old| {
                    old.mesh.quads.len() == mesh.quads.len()
                        && old
                            .mesh
                            .quads
                            .iter()
                            .zip(&mesh.quads)
                            .all(|(a, b)| a.blocks[3..] == b.blocks[3..])
                })
                .map_or(self.next_revision + at as u64, |old| old.revision);
            candidates.push((
                job.ticket,
                RegionMesh {
                    mesh,
                    ranges,
                    revision,
                    available: job.unavailable == 0,
                },
            ));
        }
        self.next_revision = next;
        for (ticket, resource) in candidates {
            let index = self
                .jobs
                .iter()
                .position(|job| job.ticket == ticket)
                .expect("候选构建不更换job");
            let job = self.jobs.remove(index);
            self.captures.retain(|capture| capture.ticket != job.ticket);
            let partial = job
                .sections
                .iter()
                .map(|(_, mesh)| mesh.quads.len())
                .sum::<usize>();
            self.retained_quads -= partial;
            let count = resource.mesh.quads.len();
            if self.state.complete(ticket, resource).is_ok() {
                self.retained_quads += count;
            }
        }
        Ok(())
    }

    fn reject_token(&mut self, patch: Patch) -> Result<(), Error> {
        match self.state.reject_patch(patch.token) {
            Ok(()) | Err(cm::StateError::StalePatch) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Remove保留最近已解析窗口，过期中间前沿允许淘汰；失败时旧GPU资源仍有效。
    pub(crate) fn prepare(&mut self) -> Result<bool, Error> {
        let bounds = self.state.config().bounds().expect("有界region域");
        let mut index = 0;
        while index < self.pending.len() {
            let patch = self.pending[index];
            let valid = self.state.validate_patch(patch.token).is_ok()
                && match patch.change {
                    Change::Activate(value) | Change::Replace(value) => bounds.intersects(value),
                    _ => false,
                };
            if valid {
                index += 1;
            } else {
                let patch = self.pending.swap_remove(index);
                self.reject_token(patch)?;
            }
        }
        let mut index = 0;
        while index < self.removes.len() {
            if self.state.validate_patch(self.removes[index].token).is_ok() {
                index += 1;
            } else {
                let patch = self.removes.swap_remove(index);
                self.reject_token(patch)?;
            }
        }
        let resolved = self
            .state
            .prepared_visible()
            .filter(|(_, value, _)| bounds.intersects(*value))
            .count()
            == volume(bounds);
        self.next_resolved = resolved.then_some(bounds);
        let mut index = 0;
        while index < self.removes.len() && self.pending.len() < SWITCHES {
            let Change::Remove(value) = self.removes[index].change else {
                unreachable!("仅Remove队列");
            };
            let protected = self
                .resolved
                .is_some_and(|resolved| resolved.intersects(value));
            if resolved || !protected {
                self.pending.push(self.removes.swap_remove(index));
            } else {
                index += 1;
            }
        }
        if !self.pending.is_empty() {
            self.dirty = true;
        }
        self.rebuild_view();
        Ok(self.dirty)
    }

    fn rebuild_view(&mut self) {
        self.view.clear();
        for (handle, _, resource) in self.state.prepared_visible() {
            if !resource.mesh.quads.is_empty() {
                self.view.push(View {
                    handle,
                    prepared: true,
                });
            }
        }
        for patch in &self.removes {
            let Change::Remove(value) = patch.change else {
                unreachable!("仅Remove队列");
            };
            let handle = self.state.find(value).expect("pending移除保持节点活动");
            if let Some(resource) = self.state.resource(handle)
                && !resource.mesh.quads.is_empty()
            {
                self.view.push(View {
                    handle,
                    prepared: false,
                });
            }
        }
        self.view.sort_unstable_by_key(|entry| {
            self.state.chunk(entry.handle).expect("视图句柄保活").coord
        });
    }
    pub(crate) fn commit(&mut self) -> Result<(), Error> {
        for patch in &self.pending {
            self.state.validate_patch(patch.token)?;
        }
        for patch in self.pending.drain(..) {
            self.state.publish(patch.token, 0)?;
        }
        if let Some(bounds) = self.next_resolved.take() {
            self.resolved = Some(bounds);
        }
        self.dirty = false;
        self.drain_retired();
        Ok(())
    }
    pub(crate) fn reject(&mut self) -> Result<(), Error> {
        while let Some(patch) = self.pending.pop() {
            self.reject_token(patch)?;
        }
        self.next_resolved = None;
        self.rebuild_view();
        Ok(())
    }
    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = ([i32; 3], u64, &Mesh)> {
        self.view.iter().map(|entry| {
            let value = self.state.chunk(entry.handle).expect("视图句柄保活");
            let resource = if entry.prepared {
                self.state.prepared_resource(entry.handle)
            } else {
                self.state.resource(entry.handle)
            }
            .expect("视图资源保活");
            (key(value), resource.revision, &resource.mesh)
        })
    }
    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        self.dirty
            || self.target.is_none()
            || self.planner_pending
            || self.scan.is_some()
            || !self.captures.is_empty()
            || !self.jobs.is_empty()
            || !self.pending.is_empty()
            || !self.removes.is_empty()
            || self.state.retired_len() != 0
            || self.state.pending_requests() != 0
            || self.state.pending_patches() != 0
    }

    #[cfg(test)]
    fn apply_legacy(&mut self, sources: &[Section<'_>]) -> Result<(), Error> {
        let anchor = self.anchor;
        let origin = sources[0].key.map(|v| f64::from(v) * 16.0);
        let camera = WorldCamera {
            origin,
            forward: [0., 0., 1., 0.],
            right: [1., 0., 0., 0.],
            up: [0., 1., 0., 0.],
        };
        let base_y = sources[0].key[1].div_euclid(4) * 4;
        for source in sources {
            self.invalidate_section(source.key)?;
        }
        let mut remaining = vec![true; sources.len()];
        for _ in 0..CAPACITY * 16 {
            let requests = self
                .plan(&camera, 0, base_y, base_y + 4, REQUESTS)?
                .to_vec();
            for request in requests {
                if let Some(index) = sources
                    .iter()
                    .enumerate()
                    .position(|(index, source)| remaining[index] && source.key == request.section)
                {
                    let source = &sources[index];
                    let proxy = Section {
                        id: request.id,
                        key: source.key,
                        available: source.available,
                        blocks: source.blocks.clone(),
                        quads: source.quads,
                    };
                    self.complete_sources(std::slice::from_ref(&proxy))?;
                    remaining[index] = false;
                } else {
                    let source = Section {
                        id: request.id,
                        key: request.section,
                        available: true,
                        blocks: Vec::new(),
                        quads: 0,
                    };
                    self.complete_sources(std::slice::from_ref(&source))?;
                }
            }
            if self.prepare()? {
                self.commit()?;
            }
            let published = sources.iter().all(|source| {
                self.state
                    .find(chunk(region(source.key)))
                    .is_some_and(|handle| {
                        self.state.is_visible(handle) && self.state.replacement(handle).is_none()
                    })
            });
            if remaining.iter().all(|value| !value) && published {
                self.rebase(anchor);
                return Ok(());
            }
        }
        Err(Error::Input("测试PRT1捕获未收敛"))
    }
    #[cfg(test)]
    pub(crate) fn merge(&self) -> Mesh {
        let mut result = Mesh::default();
        for (key, _, mesh) in self.entries() {
            let offset = std::array::from_fn(|axis| {
                (f64::from(key[axis]) * 64.0 - self.anchor[axis]) as f32
            });
            result.quads.extend(mesh.quads.iter().map(|quad| {
                let mut quad = *quad;
                quad.translate(offset);
                quad
            }));
        }
        result
    }
}

#[cfg(test)]
#[path = "chunks_tests.rs"]
mod tests;
