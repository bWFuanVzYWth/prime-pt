//! 64m region是唯一LOD0渲染资源，顺序串接4³个16m section分片，一个非空region一个BLAS。
//!
//! 目标为Minecraft水平section列视距闭区间向外region对齐，Y为完整半开世界高度域；
//! 引擎球只提供优先级，有限Bounds精确选域。域外Y明确为空，未回复不猜空。
//! State独占已完成分片与替换候选，job只拥有dirty输出。每section四个slab，非空片段
//! 稀疏驻留；准备时MeshView借旧+dirty，GPU接受后才移动未变Vec。未发布候选失效时
//! 移回恢复所有者，保留不受实际来源差分影响的输出；不复制Quad或提前撤旧覆盖。
//!
//! 默认resident1GiB、staging256MiB、单region256MiB，均为96B quad逻辑payload。
//! 源缓存、Vec余量、metadata/分配器/线程及GPU另计，不宣称RSS或显存总上限。
//! resident/region不足形成明确BudgetExcluded并暴露Pressure，Unavailable保留来源负缓存；
//! 缺源section保留旧片段，同批已完成片段可独立更新；提高预算或来源恢复后重试。
//! 过渡压力按任务最低输出空间暂缓，不阻止其它可执行小任务和Air证明。
//! 相同region身份仅在新输出完整且GPU接受后替换；完全退出当前Bounds的资源可独立卸载，
//! 不等待边缘不可用来源或整窗就绪。负缓存恢复重新编译受影响的section。
//! State从256节点按真实容量阻碍增长，空的大视距不预分配整窗metadata。
//!
//! WorkBudget分别限制管理器逻辑步、slab内核、发布和各维护队列；同轮源包共享内核与
//! ready/退役/颜色余额。每轮最多新签发8个region并保留64条待捕；等待源或容量的
//! job停放在稀疏索引中，不占用下一轮签发额度。Air证明无需内核token。
//! 全部CPU内核同步join，额度不是墙钟保证。预算计数O(1)，单项变更只更新对应region；
//! 稳态不扫描State。几何成员变化会预建完整有序目录，纯颜色变化只生成实际patch。

#[path = "chunks_capture.rs"]
mod chunks_capture;
#[path = "chunks_jobs.rs"]
mod chunks_jobs;

use super::{Mesh, Section, WorldCamera, compile};
use crate::ledger::{Contribution, Ledger};
use crate::region::{BindingColor, FragmentBindings, PartialRegion, RegionMesh};
use crate::{Budget, Error, Pressure, PressureKind};
use crate::{SourceChange, WorkBudget, WorkProgress, WorkStats};
use prime_scene::{FragmentMask, MeshParts};
use prime_scene::{MeshView, RenderRegion};
use rayon::prelude::*;
use simple_engine::chunk_manager::{
    self as cm, Bounds, Change, Chunk, Handle, Patch, State, Target, Ticket,
};
use std::collections::{BTreeMap, VecDeque};
#[path = "capacity.rs"]
mod capacity;
#[path = "compact.rs"]
mod compact;
#[cfg(test)]
#[path = "compact_tests.rs"]
mod compact_tests;
#[cfg(test)]
#[path = "publication_failure_tests.rs"]
mod publication_failure_tests;
pub use compact::PreparedGeneration;
#[path = "view.rs"]
mod view;
use view::{View, ViewChange};
#[path = "owner.rs"]
mod owner;
use owner::{fragments, sections as fragment_sections};
#[path = "change_runtime.rs"]
mod change_runtime;
#[cfg(any(test, feature = "test-fixtures"))]
const CAPACITY: usize = 8192;
const INITIAL_CAPACITY: usize = 256;
const REQUESTS: usize = 64;
const JOBS: usize = 8;
const SWITCHES: usize = 64;

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
    source_stamp: Option<crate::store::CaptureStamp>,
    ticket: Option<Ticket>,
    slot: usize,
}

struct RegionJob {
    invalidated: bool,
    interactive: bool,
    epoch: u64,
    ticket: Ticket,
    handle: Handle,
    region: [i32; 3],
    dirty: u64,
    issued: u64,
    received: u64,
    unavailable: u64,
    sections: Vec<(usize, Mesh)>,
    projected: usize,
    changed: FragmentMask,
    compiled: FragmentMask,
    verify: FragmentMask,
    finished: FragmentMask,
    tint_mask: FragmentMask,
    bindings: Vec<FragmentBindings>,
    tint_patches: Vec<prime_scene::TintPatch>,
    color_updates: Vec<BindingColor>,
    pending_tints: usize,
    pending_tint_mask: FragmentMask,
}

#[derive(Clone, Copy, Default)]
struct Dirty {
    handle: Option<Handle>,
    mask: FragmentMask,
    tints: FragmentMask,
    interactive: bool,
}

/// 16m来源与64m LOD0渲染资源的唯一CPU所有者；GPU成功后才commit。
pub struct Scene {
    state: State<RegionMesh>,
    batch: cm::Plan,
    pending: Vec<Patch>,
    parked: Vec<Patch>,
    removes: Vec<Patch>,
    view: Vec<View>,
    view_changes: Vec<ViewChange>,
    view_scratch: Vec<View>,
    view_len: usize,
    view_membership_changed: bool,
    jobs: Vec<RegionJob>,
    job_tickets: std::collections::HashMap<Ticket, usize>,
    job_handles: std::collections::HashMap<Handle, usize>,
    completion_jobs: BTreeMap<usize, Ticket>,
    tint_jobs: crate::priority::PriorityQueue<Ticket>,
    edit_refreshes: crate::ready_queue::ReadyQueue<Handle>,
    interactive_jobs: usize,
    recovered: BTreeMap<[i32; 3], PartialRegion>,
    candidate_sources: BTreeMap<[i32; 3], Vec<([i32; 3], crate::store::SourceStamp)>>,
    candidate_epochs: BTreeMap<[i32; 3], (u64, bool)>,
    input_epoch: Option<u64>,
    masks: BTreeMap<[i32; 3], Dirty>,
    captures: Vec<PendingCapture>,
    output: Vec<CaptureRequest>,
    pool: rayon::ThreadPool,
    target: Option<Target>,
    height: [i32; 2],
    anchor: [f64; 3],
    next_wire: u64,
    next_revision: u64,
    next_content: u64,
    ledger: Ledger,
    capacity: capacity::Capacity,
    work_budget: WorkBudget,
    progress: WorkProgress,
    total_progress: WorkProgress,
    turn: u64,
    mesh_remaining: usize,
    ready_remaining: usize,
    retirement_remaining: usize,
    tint_remaining: usize,
    publication_remaining: usize,
    publication_retry: [Option<Change>; 2],
    publication_work: [usize; 2],
    prepared_publication_work: [usize; 2],
    ready_used: usize,
    tint_used: usize,
    retirement_used: usize,
    invalidations: VecDeque<Handle>,
    retry_handles: VecDeque<Handle>,
    tinted: BTreeMap<[i32; 3], FragmentMask>,
    tinted_spatial: crate::spatial::SpatialIndex,
    custom_tinted: BTreeMap<[i32; 3], FragmentMask>,
    tint_sweep: change_runtime::TintSweep,
    custom_sweep: change_runtime::TintSweep,
    queued_changes: VecDeque<SourceChange>,
    edit_sources: std::collections::BTreeSet<[i32; 3]>,
    pending_raw: usize,
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

fn configuration(
    bounds: Bounds,
    target: Target,
    capacity: usize,
) -> Result<cm::Config, cm::ConfigError> {
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
        capacity,
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
    /// 创建独占工作池及可增长的稀疏State，使用默认逻辑payload预算。
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
            state: State::new(configuration(
                initial_bounds(),
                Target::new([0; 3])?,
                INITIAL_CAPACITY,
            )?),
            batch: cm::Plan::with_capacity(JOBS, SWITCHES),
            pending: Vec::with_capacity(SWITCHES),
            parked: Vec::with_capacity(SWITCHES),
            removes: Vec::with_capacity(SWITCHES),
            view: Vec::new(),
            view_changes: Vec::new(),
            view_scratch: Vec::new(),
            view_len: 0,
            view_membership_changed: false,
            jobs: Vec::with_capacity(JOBS),
            job_tickets: std::collections::HashMap::new(),
            job_handles: std::collections::HashMap::new(),
            completion_jobs: BTreeMap::new(),
            tint_jobs: crate::priority::PriorityQueue::default(),
            edit_refreshes: crate::ready_queue::ReadyQueue::default(),
            interactive_jobs: 0,
            recovered: BTreeMap::new(),
            candidate_sources: BTreeMap::new(),
            candidate_epochs: BTreeMap::new(),
            input_epoch: Some(1),
            masks: BTreeMap::new(),
            captures: Vec::with_capacity(REQUESTS),
            output: Vec::with_capacity(REQUESTS),
            pool,
            target: None,
            height: [0, 4],
            anchor: [0.; 3],
            next_wire: 1,
            next_revision: 1,
            next_content: 1,
            ledger: Ledger::default(),
            capacity: capacity::Capacity::default(),
            work_budget: WorkBudget::default(),
            progress: WorkProgress::default(),
            total_progress: WorkProgress::default(),
            turn: 0,
            mesh_remaining: WorkBudget::default().mesh_slabs,
            ready_remaining: WorkBudget::default().maintenance,
            retirement_remaining: WorkBudget::default().maintenance,
            tint_remaining: WorkBudget::default().maintenance,
            publication_remaining: WorkBudget::default().publications,
            publication_retry: [None; 2],
            publication_work: [0; 2],
            prepared_publication_work: [0; 2],
            ready_used: 0,
            tint_used: 0,
            retirement_used: 0,
            invalidations: VecDeque::new(),
            retry_handles: VecDeque::new(),
            tinted: BTreeMap::new(),
            tinted_spatial: crate::spatial::SpatialIndex::default(),
            custom_tinted: BTreeMap::new(),
            tint_sweep: change_runtime::TintSweep::default(),
            custom_sweep: change_runtime::TintSweep::default(),
            queued_changes: VecDeque::new(),
            edit_sources: std::collections::BTreeSet::new(),
            pending_raw: 0,
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

    /// 等待私有工作池全部线程执行一次屏障；仅供暖池测量排除启动阶段。
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn prepare_workers(&self) {
        self.pool.broadcast(|_| ());
    }

    /// 仅世界身份域替换清场；wire计数与几何世代不归零。
    pub fn clear(&mut self) {
        let state = State::new(
            configuration(
                initial_bounds(),
                Target::new([0; 3]).expect("零目标"),
                INITIAL_CAPACITY,
            )
            .expect("初始固定配置"),
        );
        self.clear_with(state);
    }

    fn clear_with(&mut self, state: State<RegionMesh>) {
        self.state = state;
        self.batch.clear();
        self.pending.clear();
        self.parked.clear();
        self.publication_retry = [None; 2];
        self.removes.clear();
        self.view.clear();
        self.jobs.clear();
        self.job_tickets.clear();
        self.job_handles.clear();
        self.completion_jobs.clear();
        self.tint_jobs.clear();
        self.edit_refreshes.clear();
        self.interactive_jobs = 0;
        self.recovered.clear();
        self.candidate_sources.clear();
        self.candidate_epochs.clear();
        self.masks.clear();
        self.captures.clear();
        self.output.clear();
        self.target = None;
        self.ledger.clear();
        self.capacity = capacity::Capacity::default();
        self.view_changes.clear();
        self.view_scratch.clear();
        self.view_len = 0;
        self.view_membership_changed = false;
        self.invalidations.clear();
        self.retry_handles.clear();
        self.tinted.clear();
        self.tinted_spatial.clear();
        self.custom_tinted.clear();
        self.tint_sweep = change_runtime::TintSweep::default();
        self.custom_sweep = change_runtime::TintSweep::default();
        self.queued_changes.clear();
        self.edit_sources.clear();
        self.pending_raw = 0;
        self.scan = None;
        self.scan_again = false;
        self.planner_pending = true;
        self.dirty = true;
        self.compact.clear(self.compact.generation);
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
        let bounds = Bounds::new(minimum, maximum)?;
        let target = Target::new(camera.origin.map(|v| (v / 64.0).floor() as i64))?;
        let previous_bounds = self.state.config().bounds();
        self.state.reconfigure(configuration(
            bounds,
            target,
            self.state.config().capacity(),
        )?)?;
        self.ledger.set_bounds(bounds);
        if previous_bounds != Some(bounds) {
            self.cancel_returned_removals(bounds)?;
            if let Some(previous) = previous_bounds {
                for edge in crate::spatial::difference(previous, bounds) {
                    for key in self.ledger.keys(edge) {
                        if let Some(entry) = self.ledger.contribution(key) {
                            self.capacity.pause_wait(entry.handle);
                            if self.candidate_sources.contains_key(&key) {
                                if self.state.replacement(entry.handle).is_some()
                                    || !self.state.is_visible(entry.handle)
                                        && self.state.resource(entry.handle).is_some()
                                {
                                    self.revoke_upsert(entry.handle)?;
                                    if let Some(candidate) =
                                        self.state.take_unpublished(entry.handle)?
                                    {
                                        self.ledger.owned -= candidate.owned_quads();
                                        self.refresh_ledger(entry.handle);
                                    }
                                }
                                self.remove_candidate_sources(key);
                            }
                        }
                        if let Some(partial) = self.recovered.remove(&key) {
                            self.ledger.owned -= partial.parts.len();
                        }
                    }
                }
                for edge in crate::spatial::difference(bounds, previous) {
                    for key in self.ledger.keys(edge) {
                        if let Some(entry) = self.ledger.contribution(key)
                            && self
                                .state
                                .resource(entry.handle)
                                .is_some_and(|resource| resource.excluded)
                        {
                            self.capacity.activate_wait(entry.handle);
                        }
                    }
                }
            }
            self.retry_handles.clear();
            self.retry_handles.extend(
                self.ledger
                    .excluded_handles()
                    .filter(|&handle| !self.capacity.has_wait(handle)),
            );
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
        // Sources完整失效也会清窗口；相同几何Bounds仍须恢复这一阶段前提。
        let mut minimum = bounds.minimum().map(|v| v * 4 - 1);
        let mut maximum = bounds.maximum().map(|v| v * 4 + 1);
        minimum[1] = i64::from(min_y);
        maximum[1] = i64::from(max_y);
        let source_bounds = Bounds::new(minimum, maximum)?;
        self.compact.set_window(source_bounds);
        self.captures.retain(|capture| {
            capture.ticket.is_some()
                || crate::spatial::contains(source_bounds, capture.wire.section)
        });
        self.target = Some(target);
        self.rebase(anchor);
        Ok(target)
    }

    fn mark_dirty(&mut self, handle: Handle, mask: u64) {
        self.mark_fragments(handle, fragments(mask), FragmentMask::empty());
    }
    fn mark_fragments(&mut self, handle: Handle, mask: FragmentMask, tints: FragmentMask) {
        let Some(value) = self.state.chunk(handle) else {
            return;
        };
        let value = self.masks.entry(key(value)).or_default();
        if value.handle != Some(handle) {
            *value = Dirty {
                handle: Some(handle),
                ..Dirty::default()
            };
        }
        value.mask = FragmentMask::from_words(std::array::from_fn(|i| {
            value.mask.words()[i] | mask.words()[i]
        }));
        value.tints = FragmentMask::from_words(std::array::from_fn(|i| {
            value.tints.words()[i] | tints.words()[i]
        }));
    }

    fn drop_job(&mut self, index: usize) -> RegionJob {
        let job = self.remove_job(index);
        self.compact.forget_job(job.ticket, job.pending_tints != 0);
        self.release_captures(job.ticket);
        self.ledger.owned -= job
            .sections
            .iter()
            .map(|(_, mesh)| mesh.quads.len())
            .sum::<usize>();
        self.mark_dirty(job.handle, job.dirty);
        if job.interactive
            && let Some(mask) = self.masks.get_mut(&job.region)
        {
            mask.interactive = true;
        }
        self.refresh_ledger(job.handle);
        job
    }

    fn invalidate_handle(&mut self, handle: Handle, mask: u64) -> Result<(), Error> {
        self.mark_dirty(handle, mask);
        if let Some(index) = self.handle_job_index(handle) {
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
        self.apply_changes(&[SourceChange::SectionSource(value)])
    }

    /// 通知丢失时刷新真实源；保留已发布覆盖，维护额度控制后续传播。
    pub fn invalidate_all(&mut self) {
        self.invalidate_input_epoch();
        self.compact.clear(self.compact.generation);
        self.captures.clear();
        self.edit_sources.clear();
        self.queued_changes.clear();
        self.queued_changes.push_back(SourceChange::SourceOverflow);
        self.pending_raw = self.pending_raw.saturating_add(1);
        self.planner_pending = true;
    }

    /// 只刷新持久tint绑定；不丢压缩源，不重新生成模型或流体几何。
    pub fn invalidate_tints(&mut self) {
        if !self.queued_changes.contains(&SourceChange::WorldTint) {
            self.queued_changes.push_back(SourceChange::WorldTint);
            self.pending_raw = self.pending_raw.saturating_add(1);
        }
        self.planner_pending = true;
    }
    fn scan_invalidations(&mut self) -> Result<(), Error> {
        for _ in 0..self.work_budget.maintenance {
            let Some(handle) = self.invalidations.pop_front() else {
                break;
            };
            if self.state.chunk(handle).is_some() {
                self.invalidate_handle(handle, u64::MAX)?;
            }
        }
        self.scan = (!self.invalidations.is_empty()).then_some(0);
        Ok(())
    }
    fn drain_retired(&mut self) {
        while self.retirement_remaining != 0 {
            let Some(resource) = self.state.take_retired(0) else {
                break;
            };
            self.retirement_remaining -= 1;
            self.retirement_used += 1;
            self.ledger.owned -= resource.owned_quads();
            if let Some(entry) = self.ledger.contribution(resource.key)
                && self.state.resource(entry.handle).is_none()
                && self.handle_job_index(entry.handle).is_none()
            {
                self.ledger.remove(resource.key, entry.handle);
                self.capacity.remove_wait(entry.handle);
                self.remove_candidate_sources(resource.key);
            } else if let Some(entry) = self.ledger.contribution(resource.key)
                && self.state.is_visible(entry.handle)
                && self.state.replacement(entry.handle).is_none()
            {
                self.remove_candidate_sources(resource.key);
            }
        }
        let before = self.compact.store.stats().maintenance_steps;
        let retired = self.compact.store.maintain(self.retirement_remaining);
        let steps = self.compact.store.stats().maintenance_steps - before;
        self.retirement_remaining = self.retirement_remaining.saturating_sub(steps);
        self.retirement_used += steps;
        self.progress.retired_sources += retired;
        self.retired_sources
            .extend(self.compact.store.drain_retired());
    }
}

#[cfg(test)]
#[path = "chunks_tests.rs"]
mod tests;
