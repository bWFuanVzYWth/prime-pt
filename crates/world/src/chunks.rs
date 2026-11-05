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
                                    self.reject_handle(entry.handle)?;
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

    fn release_captures(&mut self, ticket: Ticket) {
        let mut index = 0;
        while index < self.captures.len() {
            if self.captures[index].ticket != Some(ticket) {
                index += 1;
                continue;
            }
            let capture = self.captures[index].wire;
            let other = if self.compact.generation != 0 {
                self.compact.capture_owner(capture.section)
            } else {
                None
            };
            if let Some(ticket) = other {
                self.captures[index].ticket = Some(ticket);
                index += 1;
            } else {
                self.captures.remove(index);
            }
        }
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

    fn begin_region_job(&mut self, build: cm::Build) -> Result<(), Error> {
        let handle = self.state.find(build.chunk).expect("已签发region保持节点");
        self.edit_refreshes.remove(&handle);
        let mask = self.masks.remove(&key(build.chunk)).unwrap_or_default();
        let recovered = self.recovered.remove(&key(build.chunk));
        let old = self.state.resource(handle);
        let mut changed = if mask.handle == Some(handle)
            && (!mask.mask.is_empty() || !mask.tints.is_empty())
            && (old.is_some() || recovered.is_some())
        {
            mask.mask
        } else {
            FragmentMask::all()
        };
        if !changed.is_empty()
            && let Some(old) = old
        {
            if old.excluded || self.compact.generation == 0 && old.missing_sections != 0 {
                changed = FragmentMask::all();
            } else {
                changed = change_runtime::union(
                    changed,
                    fragments(old.missing_sections & fragment_sections(changed)),
                );
            }
        }
        let mut sections = Vec::new();
        let mut bindings = Vec::new();
        let mut color_updates = Vec::new();
        let mut verify = FragmentMask::empty();
        let mut tint_patches = Vec::new();
        if let Some(recovered) = recovered {
            // 缺源时没有生成新几何，恢复候选也须重新验证这些section。
            changed = change_runtime::union(changed, fragments(recovered.missing_sections));
            // 未受影响的未发布片段只转移所有权，依赖重新齐备后复用，不复制Quad。
            verify = change_runtime::subtract(recovered.changed, changed);
            for (slot, mesh) in recovered.parts {
                if changed.contains(slot) {
                    self.ledger.owned -= mesh.quads.len();
                } else {
                    sections.push((slot, mesh));
                }
            }
            bindings = recovered.bindings;
            bindings.retain(|entry| !changed.contains(entry.fragment));
            color_updates = recovered.color_updates;
            // 重染片段用下一次回填建立补丁，不能同时继承上一候选的颜色差分。
            let recolored = change_runtime::union(changed, mask.tints);
            color_updates.retain(|entry| !recolored.contains(entry.fragment));
            for group in bindings
                .iter()
                .filter(|group| !mask.tints.contains(group.fragment))
            {
                for (span, &color) in group.spans.iter().zip(&group.colors) {
                    tint_patches.push(prime_scene::TintPatch::new(
                        group.fragment,
                        span.first as usize,
                        span.count as usize,
                        color,
                    )?);
                }
            }
            if let Some(old) = self.state.resource(handle) {
                for update in &color_updates {
                    let group = old
                        .bindings
                        .iter()
                        .find(|entry| entry.fragment == update.fragment)
                        .expect("原候选已校验颜色绑定");
                    let span = group.spans[update.span];
                    tint_patches.push(prime_scene::TintPatch::new(
                        update.fragment,
                        span.first as usize,
                        span.count as usize,
                        update.argb,
                    )?);
                }
            }
            changed = change_runtime::union(changed, verify);
        }
        let dirty = fragment_sections(changed);
        let mut job = RegionJob {
            invalidated: false,
            interactive: mask.handle == Some(handle) && mask.interactive,
            epoch: self
                .input_epoch
                .ok_or(Error::Input("输入世代耗尽，需重建Scene所有者"))?,
            ticket: build.ticket,
            handle,
            region: key(build.chunk),
            dirty,
            issued: 0,
            received: 0,
            unavailable: 0,
            projected: self
                .state
                .resource(handle)
                .map_or(0, |old| old.len() - old.fragment_quads(changed))
                + sections
                    .iter()
                    .map(|(_, mesh)| mesh.quads.len())
                    .sum::<usize>(),
            sections,
            changed,
            compiled: verify,
            verify,
            finished: FragmentMask::empty(),
            tint_mask: mask.tints,
            bindings,
            tint_patches,
            color_updates,
            pending_tints: 0,
            pending_tint_mask: FragmentMask::empty(),
        };
        for slot in 0..64 {
            let y = section(job.region, slot)[1];
            if y < self.height[0] || y >= self.height[1] {
                job.received |= (1 << slot) & dirty;
                for fragment in slot * 4..slot * 4 + 4 {
                    if changed.contains(fragment) {
                        job.compiled.set(fragment)?;
                        job.finished.set(fragment)?;
                    }
                }
            }
        }
        self.insert_job(job)?;
        self.refresh_ledger(handle);
        Ok(())
    }

    /// 视距单位为16m水平列；Y为半开世界section域。返回仍待宿主捕获的请求前缀。
    ///
    /// 分帧回复共享region ticket；每轮至多8个新region，停车任务由依赖事件唤醒。
    /// 近处实际编辑可先使用部分签发额度，持续竞争时给普通任务保留推进机会。
    /// 输入超域保留旧状态；max_requests最多64且同时在途最多64，未完成请求可延后回复。
    pub fn plan(
        &mut self,
        camera: &WorldCamera,
        view_distance: u32,
        min_section_y: i32,
        max_section_y_exclusive: i32,
        max_requests: usize,
    ) -> Result<&[CaptureRequest], Error> {
        self.input_epoch
            .ok_or(Error::Input("输入世代耗尽，需重建Scene所有者"))?;
        self.begin_work_turn();
        let target = self.select(
            camera,
            view_distance,
            min_section_y,
            max_section_y_exclusive,
        )?;
        self.reject()?;
        self.output.clear();
        self.prioritize_edit_sources(camera)?;
        self.process_changes()?;
        self.drain_retired();
        self.wake_capacity_retries()?;
        self.scan_invalidations()?;
        for _ in 0..self.work_budget.maintenance {
            let Some(handle) = self.retry_handles.pop_front() else {
                break;
            };
            if self
                .state
                .resource(handle)
                .is_some_and(|resource| resource.excluded)
            {
                self.invalidate_handle(handle, u64::MAX)?;
            }
        }
        self.retry_excluded = (!self.retry_handles.is_empty()).then_some(0);
        let owned_limit = self.staging_limit(false) / size_of::<prime_scene::Quad>();
        if self.pressure.kind == PressureKind::Transition
            && self.ledger.owned + self.blocked_quads <= owned_limit
        {
            self.pressure.kind = PressureKind::None;
            self.blocked_quads = 0;
        }
        let mut edit_attempts = 0;
        let mut edit_requests = 0;
        let bounds = self.state.config().bounds().expect("有界目标");
        for _ in 0..(JOBS / 2).min(self.work_budget.planner_visits) {
            let Some(handle) = self.edit_refreshes.pop_first() else {
                break;
            };
            edit_attempts += 1;
            if self.handle_job_index(handle).is_some()
                || !self
                    .state
                    .chunk(handle)
                    .is_some_and(|chunk| bounds.intersects(chunk))
            {
                continue;
            }
            match self.state.request_refresh(handle) {
                Ok(build) => {
                    self.begin_region_job(build)?;
                    edit_requests += 1;
                }
                Err(cm::StateError::PendingPatch | cm::StateError::Transition) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.reserve_publication_retry(bounds)?;
        let hold_edit_slot = self.interactive_jobs != 0 && self.turn % 4 != 0;
        let result = cm::plan(
            &mut self.state,
            target,
            cm::Budget {
                visits: self.work_budget.planner_visits - edit_attempts,
                requests: JOBS - edit_requests,
                switches: self
                    .publication_remaining
                    .min(SWITCHES)
                    .saturating_sub(self.pending.len())
                    .saturating_sub(usize::from(hold_edit_slot)),
            },
            &mut self.batch,
        );
        for at in 0..self.batch.cancellations().len() {
            let ticket = self.batch.cancellations()[at];
            if let Some(index) = self.job_index(ticket) {
                self.drop_job(index);
            }
        }
        for request_index in 0..self.batch.requests().len() {
            self.begin_region_job(self.batch.requests()[request_index])?;
        }
        for patch in self.batch.switches() {
            if matches!(patch.change, Change::Remove(_)) {
                self.removes.push(*patch);
            } else {
                self.pending.push(*patch);
            }
        }
        self.batch.clear();
        let stats = result?;
        self.planner_pending = stats.pending;
        if stats.capacity_pressure {
            self.grow()?;
        }
        let count = max_requests.min(REQUESTS);
        let first_wire = self.next_wire;
        let additional = count.saturating_sub(self.captures.len());
        self.next_wire
            .checked_add(additional as u64)
            .ok_or(Error::Input("捕获请求编号溢出"))?;
        let bounds = self.state.config().bounds().expect("有界region域");
        if self.compact.generation == 0 {
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
                        source_stamp: None,
                        ticket: Some(job.ticket),
                        slot,
                    });
                }
            }
        }
        if self.compact.generation != 0 {
            self.issue_compact_captures(count)?;
        }
        self.output
            .extend(self.captures.iter().take(count).map(|capture| capture.wire));
        self.progress.source_requests += (self.next_wire - first_wire) as usize;
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
            if let Some(index) = capture.ticket.and_then(|ticket| self.job_index(ticket)) {
                let job = &mut self.jobs[index];
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
                if bounds.intersects(chunk(region(source.key)))
                    && let Some(ticket) = capture.ticket
                {
                    selected.push((source, ticket, capture.slot));
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
            let mut changed_handle = None;
            if let Some(index) = self.job_index(ticket) {
                let job = &mut self.jobs[index];
                self.ledger.owned += mesh.quads.len();
                job.projected += mesh.quads.len();
                job.sections.push((slot * 4, mesh));
                for fragment in slot * 4..slot * 4 + 4 {
                    job.compiled.set(fragment)?;
                    job.finished.set(fragment)?;
                }
                job.received |= 1 << slot;
                if !source.available {
                    job.unavailable |= 1 << slot;
                }
                changed_handle = Some(job.handle);
            }
            self.queue_completion(ticket);
            if let Some(handle) = changed_handle {
                self.refresh_ledger(handle);
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
        let required = if self.compact.generation == 0 {
            self.estimated_resident(&[])
        } else {
            self.ledger.resident()
        }
        .saturating_mul(size_of::<prime_scene::Quad>());
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
        self.retry_handles.clear();
        self.retry_handles.extend(
            self.ledger
                .excluded_handles()
                .filter(|&handle| !self.capacity.has_wait(handle)),
        );
        self.retry_excluded = (!self.retry_handles.is_empty()).then_some(0);
        self.planner_pending = true;
        Ok(())
    }

    /// 当前压力与被排除数量；BudgetExcluded不当作缺失源或真实空资源。
    pub fn pressure(&self) -> Pressure {
        let mut value = self.pressure;
        value.excluded_regions = self.ledger.excluded();
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
        let required = if self.compact.generation == 0 {
            self.ledger.projected()
        } else {
            self.ledger.resident()
        }
        .saturating_mul(size_of::<prime_scene::Quad>());
        if required > self.budget.resident_bytes && value.kind != PressureKind::Transition {
            value.kind = PressureKind::Resident;
            value.required_bytes = required;
            value.limit_bytes = self.budget.resident_bytes;
        } else if value.excluded_regions == 0 && value.kind != PressureKind::Transition {
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
        let resource = RegionMesh::candidate(
            job.region,
            old,
            MeshParts::default(),
            FragmentMask::empty(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            old.map_or(revision, |old| old.revision),
            old.map_or(content_revision, |old| old.content_revision),
            old.map_or(0, |old| old.missing_sections),
            true,
        )?;
        let _ = self.state.complete(job.ticket, resource);
        self.refresh_ledger(job.handle);
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
            return self.state.resource(job.handle).map_or(0, RegionMesh::len);
        }
        job.projected
            + sources
                .iter()
                .filter(|(_, ticket, _)| *ticket == job.ticket)
                .map(|(source, _, _)| source.quads)
                .sum::<usize>()
    }

    fn estimated_resident(&self, sources: &[(&Section<'_>, Ticket, usize)]) -> usize {
        let bounds = self.state.config().bounds().expect("有界目标");
        let mut total = self.ledger.projected();
        let mut seen = std::collections::HashSet::new();
        for &(_, ticket, _) in sources {
            if !seen.insert(ticket) {
                continue;
            }
            let Some(index) = self.job_index(ticket) else {
                continue;
            };
            let job = &self.jobs[index];
            if bounds.intersects(chunk(job.region)) {
                total = total
                    - self
                        .ledger
                        .contribution(job.region)
                        .map_or(0, |entry| entry.projected)
                    + self.estimate_job(job, sources);
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
        while self.ledger.owned + incoming(sources) > limit && self.jobs.len() > 1 {
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
        if self.ledger.owned + incoming(sources) > limit {
            self.blocked_quads = incoming(sources);
            self.pressure = Pressure {
                kind: PressureKind::Transition,
                required_bytes: (self.ledger.owned + self.blocked_quads) * stride,
                limit_bytes: limit * stride,
                ..Pressure::default()
            };
            return Ok(false);
        }
        Ok(true)
    }

    fn finish_jobs(&mut self) -> Result<(), Error> {
        let count = self.completion_jobs.len();
        let first_revision = self.next_revision;
        let first_content = self.next_content;
        let next = self
            .next_revision
            .checked_add(count as u64)
            .ok_or(Error::Input("region revision溢出"))?;
        let next_content = self
            .next_content
            .checked_add(count as u64)
            .ok_or(Error::Input("内容revision溢出"))?;
        // 先保留整个序号域；后续任一候选失败也不能复用已经完成资源的revision。
        self.next_revision = next;
        self.next_content = next_content;
        let mut offset = 0;
        while let Some((_, ticket)) = self.completion_jobs.pop_first() {
            let Some(index) = self.job_index(ticket) else {
                continue;
            };
            let job = &self.jobs[index];
            let legacy_unavailable = self.compact.generation == 0 && job.unavailable != 0;
            if job.invalidated
                || Some(job.epoch) != self.input_epoch
                || (!legacy_unavailable
                    && ((job.received | job.unavailable) != job.dirty
                        || !job.tint_mask.is_empty()
                        || job.pending_tints != 0))
            {
                continue;
            }
            if self.compact.generation != 0 && !self.admit_compact_candidate(index)? {
                continue;
            }
            let job = self.remove_job(index);
            let source_stamps = self.compact.job_sources(job.ticket);
            self.compact.forget_job(job.ticket, job.pending_tints != 0);
            self.release_captures(job.ticket);
            let partial = job
                .sections
                .iter()
                .map(|(_, mesh)| mesh.quads.len())
                .sum::<usize>();
            let build = (|| -> Result<RegionMesh, Error> {
                let mut parts = MeshParts::default();
                if !legacy_unavailable {
                    for (fragment, mesh) in job.sections {
                        parts.insert(fragment, mesh)?;
                    }
                }
                let old = self.state.resource(job.handle);
                let changed = if legacy_unavailable {
                    FragmentMask::empty()
                } else {
                    change_runtime::subtract(job.changed, fragments(job.unavailable))
                };
                let (bindings, patches, colors) = if legacy_unavailable {
                    (Vec::new(), Vec::new(), Vec::new())
                } else {
                    (job.bindings, job.tint_patches, job.color_updates)
                };
                let missing_sections = if legacy_unavailable {
                    u64::MAX
                } else {
                    (old.map_or(0, |old| old.missing_sections) & !job.dirty) | job.unavailable
                };
                RegionMesh::candidate(
                    job.region,
                    old,
                    parts,
                    changed,
                    bindings,
                    patches,
                    colors,
                    first_revision + offset,
                    first_content + offset,
                    missing_sections,
                    false,
                )
            })();
            let mut resource = match build {
                Ok(resource) => resource,
                Err(error) => {
                    self.ledger.owned -= partial;
                    self.state.fail(job.ticket)?;
                    self.mark_fragments(
                        job.handle,
                        job.changed,
                        self.tinted.get(&job.region).copied().unwrap_or_default(),
                    );
                    self.state.invalidate(job.handle)?;
                    self.refresh_ledger(job.handle);
                    self.planner_pending = true;
                    return Err(error);
                }
            };
            let old = self.state.resource(job.handle);
            if let Some(old) = old {
                let (same_geometry, same_content) = resource.same_output(old);
                if same_geometry {
                    resource.revision = old.revision;
                }
                if same_content {
                    resource.content_revision = old.content_revision;
                }
            }
            if legacy_unavailable {
                self.ledger.owned -= partial;
            }
            if let Err(rejected) = self.state.complete(job.ticket, resource) {
                self.ledger.owned -= rejected.resource.owned_quads();
            } else {
                self.install_candidate_sources(job.region, source_stamps);
                self.candidate_epochs
                    .insert(job.region, (job.epoch, job.interactive));
            }
            self.refresh_ledger(job.handle);
            self.refresh_tint_index(job.handle);
            if job.interactive
                && self.state.replacement(job.handle).is_some()
                && self.pending.len() < self.publication_remaining.min(SWITCHES)
                && self
                    .state
                    .config()
                    .bounds()
                    .expect("有界目标")
                    .intersects(chunk(job.region))
            {
                // 可见替换已完成，直接占用本轮剩余发布额度；仍由prepare校验并由GPU确认。
                match self.state.prepare_replace(job.handle) {
                    Ok(patch) => self.pending.push(patch),
                    Err(cm::StateError::RetirementFull) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            offset += 1;
        }
        Ok(())
    }

    fn reject_token(&mut self, patch: Patch) -> Result<(), Error> {
        match self.state.reject_patch(patch.token) {
            Ok(()) | Err(cm::StateError::StalePatch) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    /// 测量夹具的收敛条件；预算排除是已解析结果，通过pressure另行报告。
    pub fn pending(&self) -> bool {
        self.dirty
            || self.target.is_none()
            || !self.queued_changes.is_empty()
            || !self.edit_sources.is_empty()
            || !self.edit_refreshes.is_empty()
            || self.resident_retry_ready()
            || self.tint_sweep.pending()
            || self.custom_sweep.pending()
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
