//! LOD0 section调度适配；State独占Mesh、ticket与退役资源，Java仅消费捕获请求。
//!
//! 目标是随64m区格移动的12³ section盒；球只提供优先级，Bounds精确裁出窗口。
//! 交集资源和仍有效的捕获ticket保留。最近一次完整覆盖的Bounds作为保旧锚域；
//! 其outgoing资源等新窗口全部具备可发布覆盖才移除，中途未完成前沿允许预算淘汰，
//! 防止连续瞬移把有限槽位耗尽。首次加载仍需捕获，不能保证立即完整覆盖。
//!
//! 过渡联合工作集（当前、Ready、替换及退役Mesh）同受16M顶点预算约束；
//! 新旧窗口分别可容纳不代表联合工作集可容纳，超额返回资源错误并保留旧资源。
//! State/Plan及投影只存一次资源所有权；排序Handle视图和wire表均不拥有Mesh。
//! 每次plan最多1024个引擎管理步骤，另各推进64个失效扫描/退役项；prepare的
//! O(4096)覆盖与Handle投影检查、非空视图排序及源包编译不计入cm::Budget。

use super::{MAX_VERTICES, Mesh, Section, WorldCamera, compile};
use crate::Error;
use chunk_manager::{self as cm, Bounds, Change, Chunk, Handle, Patch, State, Target, Ticket};
use rayon::prelude::*;

const CAPACITY: usize = 4096;
const REQUESTS: usize = 64;
const SWITCHES: usize = 64;
const VISITS: usize = 1024;
const SCAN: usize = 64;
const WINDOW: usize = 12 * 12 * 12;

/// 24字节捕获请求；id在会话中单调，world clear后也不复用。
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
}

struct SectionMesh {
    mesh: Mesh,
    revision: u64,
}

struct View {
    handle: Handle,
    prepared: bool,
}

pub(crate) struct Scene {
    state: State<SectionMesh>,
    batch: cm::Plan,
    pending: Vec<Patch>,
    removes: Vec<Patch>,
    view: Vec<View>,
    captures: Vec<PendingCapture>,
    output: Vec<CaptureRequest>,
    pool: rayon::ThreadPool,
    target: Option<Target>,
    stable: Option<Bounds>,
    next_stable: Option<Bounds>,
    anchor: [f64; 3],
    next_wire: u64,
    next_revision: u64,
    retained_quads: usize,
    scan: Option<usize>,
    scan_again: bool,
    planner_pending: bool,
    dirty: bool,
}

fn configuration(bounds: Bounds) -> Result<cm::Config, cm::ConfigError> {
    Ok(cm::Config::new(1, 11, CAPACITY)?.with_bounds(Some(bounds)))
}

fn initial_bounds() -> Bounds {
    Bounds::new([-4; 3], [8; 3]).expect("固定非空section窗口")
}

fn chunk(key: [i32; 3]) -> Chunk {
    Chunk::new(key.map(i64::from), 0)
}

fn key(value: Chunk) -> [i32; 3] {
    value
        .coord
        .map(|v| i32::try_from(v).expect("目标输入域及12³窗口保持section坐标在i32内"))
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
            state: State::new(configuration(initial_bounds())?),
            batch: cm::Plan::with_capacity(REQUESTS, SWITCHES),
            pending: Vec::with_capacity(SWITCHES),
            removes: Vec::with_capacity(CAPACITY),
            view: Vec::with_capacity(CAPACITY),
            captures: Vec::with_capacity(REQUESTS),
            output: Vec::with_capacity(REQUESTS),
            pool,
            target: None,
            stable: None,
            next_stable: None,
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

    /// 只有世界身份域替换才清场；外部wire计数和几何世代不归零。
    pub(crate) fn clear(&mut self) {
        self.state = State::new(configuration(initial_bounds()).expect("固定配置"));
        self.batch.clear();
        self.pending.clear();
        self.removes.clear();
        self.view.clear();
        self.captures.clear();
        self.output.clear();
        self.target = None;
        self.stable = None;
        self.next_stable = None;
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

    fn select(&mut self, camera: &WorldCamera) -> Result<Target, Error> {
        let anchor = camera.anchor()?;
        let coord = camera.origin.map(|v| (v / 16.0).floor() as i64);
        let target = Target::new(coord)?;
        let base = anchor.map(|v| (v / 16.0) as i64);
        let bounds = Bounds::new(base.map(|v| v - 4), base.map(|v| v + 8))?;
        self.state.reconfigure(configuration(bounds)?)?;
        self.target = Some(target);
        self.rebase(anchor);
        Ok(target)
    }

    pub(crate) fn invalidate_section(&mut self, section: [i32; 3]) -> Result<(), Error> {
        if let Some(handle) = self.state.find(chunk(section)) {
            self.state.invalidate(handle)?;
        }
        self.planner_pending = true;
        Ok(())
    }

    /// 仅排入分步扫描；重复失效最多保留一轮额外回扫，不重置已推进的游标。
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
                self.state.invalidate(handle)?;
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

    /// 返回本次新签发的至多64条请求；Java可稍后完成，移动不撤销交集ticket。
    pub(crate) fn plan(
        &mut self,
        camera: &WorldCamera,
        max_requests: usize,
    ) -> Result<&[CaptureRequest], Error> {
        self.reject()?;
        let target = self.select(camera)?;
        self.output.clear();
        self.drain_retired();
        self.scan_invalidations()?;
        let requests = max_requests.min(REQUESTS - self.captures.len());
        self.next_wire
            .checked_add(requests as u64)
            .ok_or(Error::Input("捕获请求编号溢出"))?;
        let result = cm::plan(
            &mut self.state,
            target,
            cm::Budget {
                visits: VISITS,
                requests,
                switches: SWITCHES,
            },
            &mut self.batch,
        );
        for ticket in self.batch.cancellations() {
            self.captures.retain(|capture| capture.ticket != *ticket);
        }
        for build in self.batch.requests() {
            let wire = CaptureRequest {
                id: self.next_wire,
                section: key(build.chunk),
                reserved: 0,
            };
            self.next_wire += 1;
            self.captures.push(PendingCapture {
                wire,
                ticket: build.ticket,
            });
            self.output.push(wire);
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
        Ok(&self.output)
    }

    pub(crate) fn fail_capture(&mut self, id: u64) -> Result<(), Error> {
        if let Some(index) = self
            .captures
            .iter()
            .position(|capture| capture.wire.id == id)
        {
            let capture = self.captures.swap_remove(index);
            match self.state.fail(capture.ticket) {
                Ok(()) | Err(cm::StateError::StaleTicket) => {}
                Err(error) => return Err(error.into()),
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
        let bounds = self.state.config().bounds().expect("固定有界窗口");
        // 先核对整个包；非法id/key配对和资源超限不发布前缀。
        let mut selected = Vec::with_capacity(sources.len());
        let mut incoming = 0usize;
        for source in sources {
            if let Some(capture) = self
                .captures
                .iter()
                .find(|capture| capture.wire.id == source.id)
            {
                if capture.wire.section != source.key {
                    return Err(Error::Input("捕获id与section坐标不匹配"));
                }
                if bounds.intersects(chunk(source.key)) {
                    incoming = incoming
                        .checked_add(source.quads)
                        .ok_or(Error::Input("捕获几何数量溢出"))?;
                    selected.push((source, capture.ticket));
                }
            }
        }
        if self
            .retained_quads
            .checked_add(incoming)
            .is_none_or(|n| n > MAX_VERTICES / 4)
        {
            return Err(Error::Input("区块过渡联合Mesh超过16M顶点预算"));
        }
        let state = &self.state;
        let build = |(source, _): &(&Section<'_>, Ticket)| {
            let mesh = compile(source);
            let revision = state
                .find(chunk(source.key))
                .and_then(|handle| state.resource(handle))
                .filter(|old| {
                    old.mesh.quads.len() == mesh.quads.len()
                        && old
                            .mesh
                            .quads
                            .iter()
                            .zip(&mesh.quads)
                            .all(|(a, b)| a.blocks[3..] == b.blocks[3..])
                })
                .map_or(0, |old| old.revision);
            SectionMesh { mesh, revision }
        };
        let meshes: Vec<_> = if selected.len() < 4 || incoming < 1024 {
            selected.iter().map(build).collect()
        } else {
            self.pool
                .install(|| selected.par_iter().map(build).collect())
        };
        let fresh = meshes
            .iter()
            .filter(|entry| entry.revision == 0 && !entry.mesh.quads.is_empty())
            .count();
        let next = self
            .next_revision
            .checked_add(fresh as u64)
            .ok_or(Error::Input("section revision溢出"))?;
        for ((source, ticket), mut resource) in selected.into_iter().zip(meshes) {
            if resource.revision == 0 && !resource.mesh.quads.is_empty() {
                resource.revision = self.next_revision;
                self.next_revision += 1;
            }
            let count = resource.mesh.quads.len();
            if self.state.complete(ticket, resource).is_ok() {
                self.retained_quads += count;
            }
            self.captures.retain(|capture| capture.wire.id != source.id);
        }
        self.next_revision = next;
        for source in sources {
            if !bounds.intersects(chunk(source.key)) {
                self.fail_capture(source.id)?;
            }
        }
        self.planner_pending = true;
        Ok(())
    }

    fn reject_token(&mut self, patch: Patch) -> Result<(), Error> {
        match self.state.reject_patch(patch.token) {
            Ok(()) | Err(cm::StateError::StalePatch) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// 投影预算内候选；Remove保留最后完整覆盖，过期中途前沿可淘汰。
    pub(crate) fn prepare(&mut self) -> Result<bool, Error> {
        let bounds = self.state.config().bounds().expect("固定有界窗口");
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
        let covered = self
            .state
            .prepared_visible()
            .filter(|(_, value, _)| bounds.intersects(*value))
            .count()
            == WINDOW;
        self.next_stable = covered.then_some(bounds);
        let mut index = 0;
        while index < self.removes.len() && self.pending.len() < SWITCHES {
            let Change::Remove(value) = self.removes[index].change else {
                unreachable!("移除队列只保存Remove");
            };
            let protected = self.stable.is_some_and(|stable| stable.intersects(value));
            if covered || !protected {
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
                unreachable!("移除队列只保存Remove");
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

    /// GPU准备成功后发布已验证patch；CPU Mesh退役不管理GPU句柄生命周期。
    pub(crate) fn commit(&mut self) -> Result<(), Error> {
        for patch in &self.pending {
            self.state.validate_patch(patch.token)?;
        }
        for patch in self.pending.drain(..) {
            self.state.publish(patch.token, 0)?;
        }
        if let Some(bounds) = self.next_stable.take() {
            self.stable = Some(bounds);
        }
        self.dirty = false;
        self.drain_retired();
        Ok(())
    }

    pub(crate) fn reject(&mut self) -> Result<(), Error> {
        while let Some(patch) = self.pending.pop() {
            self.reject_token(patch)?;
        }
        self.next_stable = None;
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
            || !self.pending.is_empty()
            || !self.removes.is_empty()
            || self.state.retired_len() != 0
            || self.state.pending_requests() != 0
            || self.state.pending_patches() != 0
    }

    #[cfg(test)]
    fn apply_legacy(&mut self, sources: &[Section<'_>]) -> Result<(), Error> {
        let anchor = self.anchor;
        let origin = if let Some(target) = self.target {
            target.coords().map(|v| v as f64 * 16.0)
        } else {
            sources[0].key.map(|v| f64::from(v) * 16.0)
        };
        let camera = WorldCamera {
            origin,
            forward: [0., 0., 1., 0.],
            right: [1., 0., 0., 0.],
            up: [0., 1., 0., 0.],
        };
        for source in sources {
            self.invalidate_section(source.key)?;
        }
        let mut remaining = vec![true; sources.len()];
        for _ in 0..CAPACITY * 16 {
            let requests = self.plan(&camera, REQUESTS)?.to_vec();
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
                        blocks: source.blocks.clone(),
                        quads: source.quads,
                    };
                    self.complete_sources(std::slice::from_ref(&proxy))?;
                    remaining[index] = false;
                } else {
                    let source = Section {
                        id: request.id,
                        key: request.section,
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
                self.state.find(chunk(source.key)).is_some_and(|handle| {
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
                (f64::from(key[axis]) * 16.0 - self.anchor[axis]) as f32
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
