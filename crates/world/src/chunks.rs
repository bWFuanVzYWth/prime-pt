//! 64m region是唯一LOD0渲染资源，顺序串接4³个16m section分片，一个非空region一个BLAS。
//!
//! 目标为Minecraft水平section列视距闭区间向外region对齐，Y为完整半开世界高度域；
//! 引擎球只提供优先级，有限Bounds精确选域。域外Y明确为空，未回复不猜空。
//! State独占已完成分片与替换候选，job只拥有dirty输出。准备时MeshView借旧+dirty；
//! GPU成功且旧读取完成后，publish_with回调move未dirty Vec，不复制整个region。
//! 回调仅固定数组内所有权移动，无分配和可失败操作；拒绝前旧分片始终保留。
//!
//! 默认resident1GiB、staging256MiB、单region256MiB，均为96B quad逻辑payload。
//! 源缓存、Vec余量、metadata/分配器/线程及GPU另计，不宣称RSS或显存总上限。
//! resident/region不足形成明确BudgetExcluded并暴露Pressure，Unavailable保留来源负缓存；
//! 两者均保留已有完整覆盖，不得以空候选替换旧mesh。提高预算或来源恢复后重试。
//! 过渡压力暂停待捕前缀，可取消未发布job；容量不足允许等待，不主动撤旧制造覆盖缺口。
//! 相同region身份仅在新输出完整且GPU接受后替换；完全退出当前Bounds的资源可独立卸载，
//! 不等待边缘不可用来源或整窗就绪。恢复通知重捕全64。
//! 固定8192节点要求两倍目标域加64可容纳；已发布旧覆盖优先于收紧后的预算。
//!
//! 每plan最多1024个引擎步骤、8个在途region job、64条待捕，另各64失效/退役项。
//! 8192槽prepare与预算检查、视图排序、源编译不计入cm::Budget；全部CPU阶段同步join。

use super::{Mesh, Section, WorldCamera, compile};
use crate::region::RegionMesh;
use crate::{Budget, Error, Pressure, PressureKind};
use chunk_manager::{self as cm, Bounds, Change, Chunk, Handle, Patch, State, Target, Ticket};
use prime_scene::{MeshView, RenderRegion};
use rayon::prelude::*;
#[path = "compact.rs"]
mod compact;
#[cfg(test)]
#[path = "compact_tests.rs"]
mod compact_tests;
pub use compact::PreparedGeneration;

const CAPACITY: usize = 8192;
const REQUESTS: usize = 64;
const JOBS: usize = 8;
const SWITCHES: usize = 64;
const VISITS: usize = 1024;
const SCAN: usize = 64;

/// 每条24字节；id在会话中单调，世界clear后也不复用。坐标仍以16m section计。
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CaptureRequest {
    /// 会话内单调的捕获身份。
    pub id: u64,
    /// 绝对16m section坐标。
    pub section: [i32; 3],
    /// 固定为零的协议扩展位。
    pub reserved: u32,
}
const _: () = assert!(size_of::<CaptureRequest>() == 24);

struct PendingCapture {
    wire: CaptureRequest,
    ticket: Ticket,
    slot: usize,
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
    needed: Vec<[i32; 3]>,
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

/// 16m来源与64m LOD0渲染资源的唯一CPU所有者；GPU成功后才commit。
pub struct Scene {
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
    height: [i32; 2],
    anchor: [f64; 3],
    next_wire: u64,
    next_revision: u64,
    next_content: u64,
    retained_quads: usize,
    scan: Option<usize>,
    scan_again: bool,
    planner_pending: bool,
    dirty: bool,
    compact: compact::Sources,
    budget: Budget,
    pressure: Pressure,
    exclusion: Pressure,
    blocked_quads: usize,
    retry_excluded: Option<usize>,
    retired_sources: Vec<[i32; 3]>,
}

#[cfg(test)]
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
    /// 创建独占工作池和固定8192节点容量，使用默认逻辑payload预算。
    pub fn new() -> Result<Self, Error> {
        let threads = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(8);
        Self::with_threads(threads)
    }

    /// 显式调度层配置的正数工作线程；不使用全局池或嵌套调度。
    pub fn with_threads(threads: usize) -> Result<Self, Error> {
        if threads == 0 {
            return Err(Error::Input("工作线程数必须为正数"));
        }
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
            height: [0, 4],
            anchor: [0.; 3],
            next_wire: 1,
            next_revision: 1,
            next_content: 1,
            retained_quads: 0,
            scan: None,
            scan_again: false,
            planner_pending: true,
            dirty: true,
            compact: compact::Sources::default(),
            budget: Budget::default(),
            pressure: Pressure::default(),
            exclusion: Pressure::default(),
            blocked_quads: 0,
            retry_excluded: None,
            retired_sources: Vec::new(),
        })
    }

    /// 仅世界身份域替换清场；wire计数与几何世代不归零。
    pub fn clear(&mut self) {
        let state = State::new(
            configuration(initial_bounds(), Target::new([0; 3]).expect("零目标"))
                .expect("初始固定配置"),
        );
        self.clear_with(state);
    }

    fn clear_with(&mut self, state: State<RegionMesh>) {
        self.state = state;
        self.batch.clear();
        self.pending.clear();
        self.removes.clear();
        self.view.clear();
        self.jobs.clear();
        self.masks.fill(Dirty::default());
        self.captures.clear();
        self.output.clear();
        self.target = None;
        self.retained_quads = 0;
        self.scan = None;
        self.scan_again = false;
        self.planner_pending = true;
        self.dirty = true;
        self.compact = compact::Sources {
            generation: self.compact.generation,
            ..compact::Sources::default()
        };
        self.pressure = Pressure::default();
        self.exclusion = Pressure::default();
        self.blocked_quads = 0;
        self.retry_excluded = None;
        self.retired_sources.clear();
    }

    /// 消费实际压缩源退休事件；不扫描live keys，不包含仅内容失效的活跃源。
    /// 宿主据此退休独立tint源页，并在新tint plan之前处理依赖失效。
    pub fn drain_retired_sources(&mut self) -> impl Iterator<Item = [i32; 3]> + '_ {
        self.retired_sources.drain(..)
    }

    /// 更新相机原点，仅改变实例放置，不修改局部几何。
    pub fn rebase(&mut self, anchor: [f64; 3]) {
        if self.anchor != anchor {
            self.anchor = anchor;
            self.dirty = true;
        }
    }
    /// 本次渲染视图使用的绝对世界原点。
    pub const fn anchor(&self) -> [f64; 3] {
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
        let previous_bounds = self.state.config().bounds();
        self.state.reconfigure(configuration(bounds, target)?)?;
        if previous_bounds != Some(bounds) {
            self.retry_excluded = Some(0);
            let needed: std::collections::BTreeSet<_> = self
                .jobs
                .iter()
                .flat_map(|job| job.needed.iter().copied())
                .collect();
            let retired = &mut self.retired_sources;
            self.compact.values.retain(|key, _| {
                let fits = |bounds: Bounds| {
                    (0..3).all(|axis| {
                        i64::from(key[axis]) >= bounds.minimum()[axis] * 4 - 1
                            && i64::from(key[axis]) < bounds.maximum()[axis] * 4 + 1
                    })
                };
                let keep = fits(bounds) || needed.contains(key);
                if !keep {
                    retired.push(*key);
                }
                keep
            });
        }
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

    fn release_captures(&mut self, ticket: Ticket) {
        let bounds = self.state.config().bounds().expect("有界需求域");
        let mut index = 0;
        while index < self.captures.len() {
            if self.captures[index].ticket != ticket {
                index += 1;
                continue;
            }
            let capture = self.captures[index].wire;
            let other = if self.compact.generation != 0 {
                self.jobs.iter().find(|job| {
                    job.ticket != ticket
                        && bounds.intersects(chunk(job.region))
                        && job.needed.contains(&capture.section)
                })
            } else {
                None
            };
            if let Some(job) = other {
                self.captures[index].ticket = job.ticket;
                index += 1;
            } else {
                self.captures.remove(index);
            }
        }
    }

    fn drop_job(&mut self, index: usize) -> RegionJob {
        let job = self.jobs.remove(index);
        self.compact
            .tints
            .retain(|pending| pending.ticket != job.ticket);
        self.compact.prune_tints();
        self.release_captures(job.ticket);
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

    /// 标记实际来源变化；取消其旧job，后续重新捕获并保留旧可见输出。
    pub fn invalidate_section(&mut self, value: [i32; 3]) -> Result<(), Error> {
        if value.iter().any(|v| v.checked_mul(16).is_none()) {
            return Err(Error::Input("失效section坐标越界"));
        }
        self.compact.values.remove(&value);
        let mut regions = std::collections::BTreeMap::<[i32; 3], u64>::new();
        if self.compact.generation == 0 {
            regions.insert(region(value), 1 << slot(value));
        } else {
            for y in -1..=1 {
                for z in -1..=1 {
                    for x in -1..=1 {
                        let center = [value[0] + x, value[1] + y, value[2] + z];
                        *regions.entry(region(center)).or_default() |= 1 << slot(center);
                    }
                }
            }
        }
        for (region_key, mask) in regions {
            if let Some(handle) = self.state.find(chunk(region_key)) {
                self.invalidate_handle(handle, mask)?;
            }
        }
        Ok(())
    }

    /// 分步扫描；重复通知至多保留一轮额外回扫，不重置已推进游标。
    pub fn invalidate_all(&mut self) {
        self.compact.values.clear();
        self.invalidate_tints();
    }

    /// 只有颜色/群系依赖变化时保留压缩方块源，分步重新编译并回填真实tint。
    pub fn invalidate_tints(&mut self) {
        while !self.jobs.is_empty() {
            let handle = self.jobs[0].handle;
            self.mark_dirty(handle, u64::MAX);
            let job = self.drop_job(0);
            let _ = self.state.fail(job.ticket);
        }
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
            self.retained_quads -= resource.owned_quads();
        }
    }

    /// 视距单位为16m水平列；Y为半开世界section域。返回仍待宿主捕获的请求前缀。
    ///
    /// 分帧回复共享同一region ticket，完整64位置（含高度域证明为空的位置）才完成资源。
    /// 输入超域保留旧状态；max_requests最多64且同时在途最多64，未完成请求可延后回复。
    pub fn plan(
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
        for _ in 0..SCAN {
            let Some(index) = self.retry_excluded else {
                break;
            };
            if let Some(handle) = self.state.handle_at(index)
                && self
                    .state
                    .resource(handle)
                    .is_some_and(|region| region.excluded)
            {
                self.invalidate_handle(handle, u64::MAX)?;
            }
            self.retry_excluded = (index + 1 < CAPACITY).then_some(index + 1);
        }
        let owned_limit = (self.budget.resident_bytes + self.budget.staging_bytes)
            / size_of::<prime_scene::Quad>();
        if self.pressure.kind == PressureKind::Transition
            && self.retained_quads + self.blocked_quads <= owned_limit
        {
            self.pressure.kind = PressureKind::None;
            self.blocked_quads = 0;
        }
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
                needed: if self.compact.generation != 0 {
                    compact::needed(key(build.chunk), dirty)
                } else {
                    Vec::new()
                },
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
        let count = if self.pressure.kind == PressureKind::Transition {
            0
        } else {
            max_requests.min(REQUESTS)
        };
        let additional = count.saturating_sub(self.captures.len());
        self.next_wire
            .checked_add(additional as u64)
            .ok_or(Error::Input("捕获请求编号溢出"))?;
        let bounds = self.state.config().bounds().expect("有界region域");
        if self.compact.generation != 0 {
            for job in &self.jobs {
                if !bounds.intersects(chunk(job.region)) {
                    continue;
                }
                for key in &job.needed {
                    if self.captures.len() >= count {
                        break;
                    }
                    if key[1] < self.height[0]
                        || key[1] >= self.height[1]
                        || self.compact.values.contains_key(key)
                        || self
                            .captures
                            .iter()
                            .any(|capture| capture.wire.section == *key)
                    {
                        continue;
                    }
                    let wire = CaptureRequest {
                        id: self.next_wire,
                        section: *key,
                        reserved: 0,
                    };
                    self.next_wire += 1;
                    self.captures.push(PendingCapture {
                        wire,
                        ticket: job.ticket,
                        slot: 0,
                    });
                }
            }
        } else {
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
        }
        self.output
            .extend(self.captures.iter().take(count).map(|capture| capture.wire));
        self.finish_jobs()?;
        Ok(&self.output)
    }

    /// 放弃一条宿主捕获；后续以新wire ID重试，不复用旧回复身份。
    pub fn fail_capture(&mut self, id: u64) -> Result<(), Error> {
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

    /// 同步消费完整借用源包；协议错误保留旧发布，容量不足用pressure报告。
    pub fn apply(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let sources = super::decode(bytes)?;
        #[cfg(any(test, feature = "test-fixtures"))]
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
        if !self.admit_sources(&mut selected)? {
            return Ok(());
        }
        let incoming: usize = selected
            .iter()
            .filter(|(_, ticket, _)| !unavailable.contains(ticket))
            .map(|(source, _, _)| source.quads)
            .sum();
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

    /// 返回当前CPU逻辑payload限制；不包含GPU、源缓存、线程栈或分配器开销。
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// 更新限制并分步重试预算排除资源；非法配置保留旧限制与资源。
    pub fn set_budget(&mut self, budget: Budget) -> Result<(), Error> {
        let stride = size_of::<prime_scene::Quad>();
        if budget.resident_bytes < stride
            || budget.staging_bytes < stride
            || budget.region_bytes < stride
            || budget.region_bytes > budget.staging_bytes
            || budget.region_bytes > budget.resident_bytes
            || budget
                .resident_bytes
                .checked_add(budget.staging_bytes)
                .is_none()
        {
            return Err(Error::Input("CPU payload预算域无效"));
        }
        if self.budget == budget {
            return Ok(());
        }
        let required = self.estimated_resident(&[]) * size_of::<prime_scene::Quad>();
        self.budget = budget;
        self.pressure = Pressure::default();
        if required > budget.resident_bytes {
            self.invalidate_tints();
            self.pressure = Pressure {
                kind: PressureKind::Resident,
                required_bytes: required,
                limit_bytes: budget.resident_bytes,
                ..Pressure::default()
            };
        }
        self.blocked_quads = 0;
        self.retry_excluded = Some(0);
        self.planner_pending = true;
        Ok(())
    }

    /// 当前压力与被排除数量；BudgetExcluded不当作缺失源或真实空资源。
    pub fn pressure(&self) -> Pressure {
        let mut value = self.pressure;
        value.excluded_regions = (0..CAPACITY)
            .filter_map(|index| self.state.handle_at(index))
            .filter(|handle| {
                self.state
                    .replacement(*handle)
                    .or_else(|| self.state.resource(*handle))
                    .is_some_and(|resource| resource.excluded)
            })
            .count();
        if value.excluded_regions != 0 && value.kind != PressureKind::Transition {
            value.kind = self.exclusion.kind;
            value.required_bytes = self.exclusion.required_bytes;
            value.limit_bytes = self.exclusion.limit_bytes;
        }
        value.deferred_requests = if value.kind == PressureKind::Transition {
            self.captures.len()
        } else {
            0
        };
        let reconciling = self.scan.is_some()
            || !self.jobs.is_empty()
            || self.state.pending_requests() != 0
            || self.state.pending_patches() != 0;
        if value.excluded_regions == 0
            && value.kind != PressureKind::Transition
            && !(value.kind == PressureKind::Resident && reconciling)
        {
            value.kind = PressureKind::None;
        }
        value
    }

    fn exclude_job(
        &mut self,
        index: usize,
        kind: PressureKind,
        required: usize,
        limit: usize,
    ) -> Result<(), Error> {
        let revision = self.next_revision;
        let content_revision = self.next_content;
        let next = revision
            .checked_add(1)
            .ok_or(Error::Input("几何revision溢出"))?;
        let next_content = content_revision
            .checked_add(1)
            .ok_or(Error::Input("内容revision溢出"))?;
        let job = self.drop_job(index);
        let old = self.state.resource(job.handle);
        let resource = RegionMesh {
            parts: old.map(|_| Box::new(std::array::from_fn(|_| Mesh::default()))),
            changed: 0,
            revision: old.map_or(revision, |old| old.revision),
            content_revision: old.map_or(content_revision, |old| old.content_revision),
            available: false,
            excluded: true,
        };
        let _ = self.state.complete(job.ticket, resource);
        self.next_revision = next;
        self.next_content = next_content;
        self.pressure = Pressure {
            kind,
            required_bytes: required,
            limit_bytes: limit,
            ..Pressure::default()
        };
        self.exclusion = self.pressure;
        self.planner_pending = true;
        Ok(())
    }

    fn estimate_job(&self, job: &RegionJob, sources: &[(&Section<'_>, Ticket, usize)]) -> usize {
        if job.unavailable != 0
            || sources
                .iter()
                .any(|(source, ticket, _)| *ticket == job.ticket && !source.available)
        {
            return self
                .state
                .resource(job.handle)
                .map_or(0, |old| old.view().len());
        }
        let unchanged = self.state.resource(job.handle).map_or(0, |old| {
            old.parts()
                .iter()
                .enumerate()
                .filter(|(slot, _)| job.dirty & (1 << slot) == 0)
                .map(|(_, mesh)| mesh.quads.len())
                .sum()
        });
        unchanged
            + job
                .sections
                .iter()
                .map(|(_, mesh)| mesh.quads.len())
                .sum::<usize>()
            + sources
                .iter()
                .filter(|(_, ticket, _)| *ticket == job.ticket)
                .map(|(source, _, _)| source.quads)
                .sum::<usize>()
    }

    fn estimated_resident(&self, sources: &[(&Section<'_>, Ticket, usize)]) -> usize {
        let bounds = self.state.config().bounds().expect("有界目标");
        let mut total = 0;
        for index in 0..CAPACITY {
            if let Some(handle) = self.state.handle_at(index)
                && bounds.intersects(self.state.chunk(handle).expect("活动句柄"))
                && let Some(current) = self.state.resource(handle)
            {
                total += self
                    .state
                    .replacement(handle)
                    .map_or_else(|| current.view().len(), |next| next.over(current).len());
            }
        }
        for job in &self.jobs {
            if bounds.intersects(chunk(job.region)) {
                total -= self
                    .state
                    .resource(job.handle)
                    .map_or(0, |old| old.view().len());
                total += self.estimate_job(job, sources);
            }
        }
        total
    }

    fn admit_sources(
        &mut self,
        sources: &mut Vec<(&Section<'_>, Ticket, usize)>,
    ) -> Result<bool, Error> {
        let stride = size_of::<prime_scene::Quad>();
        let mut index = 0;
        while index < self.jobs.len() {
            let required = self.estimate_job(&self.jobs[index], sources) * stride;
            if required > self.budget.region_bytes {
                let ticket = self.jobs[index].ticket;
                self.exclude_job(
                    index,
                    PressureKind::Region,
                    required,
                    self.budget.region_bytes,
                )?;
                sources.retain(|(_, value, _)| *value != ticket);
            } else {
                index += 1;
            }
        }
        let mut resident = self.estimated_resident(sources) * stride;
        let mut index = self.jobs.len();
        while resident > self.budget.resident_bytes && index > 0 {
            index -= 1;
            let ticket = self.jobs[index].ticket;
            if !sources.iter().any(|(_, value, _)| *value == ticket)
                && self.jobs[index].sections.is_empty()
            {
                continue;
            }
            if self.estimate_job(&self.jobs[index], sources) == 0 {
                continue;
            }
            self.exclude_job(
                index,
                PressureKind::Resident,
                resident,
                self.budget.resident_bytes,
            )?;
            sources.retain(|(_, value, _)| *value != ticket);
            resident = self.estimated_resident(sources) * stride;
        }
        let limit = (self.budget.resident_bytes + self.budget.staging_bytes) / stride;
        let incoming = |sources: &[(&Section<'_>, Ticket, usize)]| {
            sources
                .iter()
                .filter(|(_, ticket, _)| {
                    !sources
                        .iter()
                        .any(|(source, value, _)| *value == *ticket && !source.available)
                })
                .map(|(source, _, _)| source.quads)
                .sum::<usize>()
        };
        // 先丢弃较远的部分job；固定保留队首一个，保证有效单region可以取得过渡余量。
        while self.retained_quads + incoming(sources) > limit && self.jobs.len() > 1 {
            let job = self.drop_job(self.jobs.len() - 1);
            let _ = self.state.fail(job.ticket);
            sources.retain(|(_, ticket, _)| *ticket != job.ticket);
        }
        // 取消较远job会恢复其旧可见资源，必须重新评估最终常驻量，不能误判为可等待的过渡。
        let mut resident = self.estimated_resident(sources) * stride;
        let mut index = self.jobs.len();
        while resident > self.budget.resident_bytes && index > 0 {
            index -= 1;
            let ticket = self.jobs[index].ticket;
            if self.estimate_job(&self.jobs[index], sources) == 0
                || !sources.iter().any(|(_, value, _)| *value == ticket)
                    && self.jobs[index].sections.is_empty()
            {
                continue;
            }
            self.exclude_job(
                index,
                PressureKind::Resident,
                resident,
                self.budget.resident_bytes,
            )?;
            sources.retain(|(_, value, _)| *value != ticket);
            resident = self.estimated_resident(sources) * stride;
        }
        if self.retained_quads + incoming(sources) > limit {
            self.blocked_quads = incoming(sources);
            self.pressure = Pressure {
                kind: PressureKind::Transition,
                required_bytes: (self.retained_quads + self.blocked_quads) * stride,
                limit_bytes: limit * stride,
                ..Pressure::default()
            };
            return Ok(false);
        }
        Ok(true)
    }

    fn finish_jobs(&mut self) -> Result<(), Error> {
        let count = self
            .jobs
            .iter()
            .filter(|job| job.unavailable != 0 || job.received == job.dirty)
            .count();
        let next = self
            .next_revision
            .checked_add(count as u64)
            .ok_or(Error::Input("region revision溢出"))?;
        let next_content = self
            .next_content
            .checked_add(count as u64)
            .ok_or(Error::Input("内容revision溢出"))?;
        let mut index = 0;
        let mut offset = 0;
        while index < self.jobs.len() {
            if self.jobs[index].unavailable == 0
                && self.jobs[index].received != self.jobs[index].dirty
            {
                index += 1;
                continue;
            }
            let job = self.jobs.remove(index);
            self.compact
                .tints
                .retain(|pending| pending.ticket != job.ticket);
            self.compact.prune_tints();
            self.release_captures(job.ticket);
            let partial = job
                .sections
                .iter()
                .map(|(_, mesh)| mesh.quads.len())
                .sum::<usize>();
            let available = job.unavailable == 0;
            let mut parts = Box::new(std::array::from_fn(|_| Mesh::default()));
            if available {
                for (slot, mesh) in job.sections {
                    parts[slot] = mesh;
                }
            }
            let old = self.state.resource(job.handle);
            let mut resource = RegionMesh {
                parts: Some(parts),
                changed: if available { job.dirty } else { 0 },
                revision: self.next_revision + offset,
                content_revision: self.next_content + offset,
                available,
                excluded: false,
            };
            if let Some(old) = old {
                let (same_geometry, same_content) = resource.same_output(old);
                if same_geometry {
                    resource.revision = old.revision;
                }
                if same_content {
                    resource.content_revision = old.content_revision;
                }
            }
            if !available {
                self.retained_quads -= partial;
            }
            if let Err(rejected) = self.state.complete(job.ticket, resource) {
                self.retained_quads -= rejected.resource.owned_quads();
            }
            offset += 1;
        }
        self.next_revision = next;
        self.next_content = next_content;
        Ok(())
    }

    fn reject_token(&mut self, patch: Patch) -> Result<(), Error> {
        match self.state.reject_patch(patch.token) {
            Ok(()) | Err(cm::StateError::StalePatch) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// 同身份替换保持旧覆盖；完全退出当前Bounds的Remove独立卸载，不以整窗就绪为门限。
    /// 来源/预算不可用仅改变等待状态，GPU接受完整候选并commit后才替换旧mesh。
    pub fn prepare(&mut self) -> Result<bool, Error> {
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
        let mut index = 0;
        while index < self.removes.len() && self.pending.len() < SWITCHES {
            let Change::Remove(value) = self.removes[index].change else {
                unreachable!("仅Remove队列");
            };
            if !bounds.intersects(value) {
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
            let current = self.state.resource(handle).expect("准备视图旧资源存在");
            let mesh = if std::ptr::eq(current, resource) {
                resource.view()
            } else {
                resource.over(current)
            };
            if !mesh.is_empty() {
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
                && !resource.view().is_empty()
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
    /// 宿主确认GPU准备成功且旧读取完成后，转移未修改分片并发布候选。
    pub fn commit(&mut self) -> Result<(), Error> {
        let before: usize = self
            .state
            .visible()
            .map(|(_, _, region)| region.view().len())
            .sum();
        let excluded = self.state.visible().any(|(_, _, region)| region.excluded);
        for patch in &self.pending {
            self.state.validate_patch(patch.token)?;
        }
        for patch in self.pending.drain(..) {
            self.state
                .publish_with(patch.token, 0, RegionMesh::inherit)?;
        }
        self.dirty = false;
        self.drain_retired();
        let after: usize = self
            .state
            .visible()
            .map(|(_, _, region)| region.view().len())
            .sum();
        if after < before && excluded {
            self.retry_excluded = Some(0);
        }
        Ok(())
    }
    /// GPU准备失败时撤销候选投影，保留旧分片与可见覆盖。
    pub fn reject(&mut self) -> Result<(), Error> {
        while let Some(patch) = self.pending.pop() {
            self.reject_token(patch)?;
        }
        self.rebuild_view();
        Ok(())
    }
    /// 非空region的稳定顺序借用；准备中的替换按旧+dirty overlay投影。
    pub fn entries(&self) -> impl ExactSizeIterator<Item = ([i32; 3], u64, MeshView<'_>)> {
        self.render_regions()
            .map(|region| (region.key, region.geometry_revision, region.mesh))
    }

    /// 借给一次GPU事务的region元数据；返回前需prepare，成功后commit，失败reject。
    pub fn render_regions(&self) -> impl ExactSizeIterator<Item = RenderRegion<'_>> {
        self.view.iter().map(|entry| {
            let value = self.state.chunk(entry.handle).expect("视图句柄保活");
            let current = self.state.resource(entry.handle).expect("视图旧资源保活");
            let resource = if entry.prepared {
                self.state.prepared_resource(entry.handle)
            } else {
                Some(current)
            }
            .expect("视图资源保活");
            let mesh = if !std::ptr::eq(resource, current) {
                resource.over(current)
            } else {
                resource.view()
            };
            RenderRegion {
                key: key(value),
                geometry_revision: resource.revision,
                content_revision: resource.content_revision,
                mesh,
            }
        })
    }
    #[cfg(any(test, feature = "test-fixtures"))]
    /// 测量夹具的收敛条件；预算排除是已解析结果，通过pressure另行报告。
    pub fn pending(&self) -> bool {
        self.dirty
            || self.target.is_none()
            || self.planner_pending
            || self.scan.is_some()
            || self.retry_excluded.is_some()
            || !self.captures.is_empty()
            || !self.jobs.is_empty()
            || !self.pending.is_empty()
            || !self.removes.is_empty()
            || self.state.retired_len() != 0
            || self.state.pending_requests() != 0
            || self.state.pending_patches() != 0
    }

    #[cfg(any(test, feature = "test-fixtures"))]
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
    #[cfg(any(test, feature = "test-fixtures"))]
    /// 测试夹具将当前借用视图复制为连续mesh；生产路径不调用。
    pub fn merge(&self) -> Mesh {
        let mut result = Mesh::default();
        for (key, _, mesh) in self.entries() {
            let offset = std::array::from_fn(|axis| {
                (f64::from(key[axis]) * 64.0 - self.anchor[axis]) as f32
            });
            result.quads.extend(mesh.quads().map(|quad| {
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
