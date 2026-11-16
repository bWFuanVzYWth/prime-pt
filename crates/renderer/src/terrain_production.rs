//! 世界保有可继续编辑的源分片。帧线程按额度把变化页送入有界通道；后台池独占工作页，
//! 共享唯一的材质世代表，并行编码、合并区域及匹配旧身份。绘制仍持有已发布的 TLAS 和 GPU 列。
//! 完成消息归还原 CPU 历史和材质表；只有对应世代、区域版本和发布根仍相符的结果可进入上传。
pub(super) use super::terrain_packets::FragmentSpan;
use super::terrain_packets::{Build, History, Piece, Tables, append_indices, encode};
use super::{Error, Gpu, PackedRegions, RegionGpu, region_match::Matched, scene_cache};
use prime_scene::{Quad, RenderView};
use rayon::prelude::*;
use std::{
    ops::Range,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    time::{Duration, Instant},
};

const PAGE: usize = 2048;
const HANDOFF_BYTES: usize = 4 * 1024 * 1024;
const HANDOFF_TIME: Duration = Duration::from_micros(750);
type Stamp = ([i32; 3], u64, u64);
type Stamps = Vec<Stamp>;
#[derive(Clone)]
struct Request {
    tint: Option<[u32; 3]>,
    region: usize,
    slot: usize,
    range: Range<usize>,
    job: usize,
    piece: usize,
}
struct SourcePage {
    job: usize,
    piece: usize,
    input: PageInput,
}
enum PageInput {
    Quads(Vec<Quad>),
    Tint(Vec<scene_cache::QuadAttributes>),
}
enum Entry {
    Reuse(usize),
    Build,
}
struct Job {
    limit: u32,
    update: scene_cache::Update<RegionGpu>,
    entries: Vec<Entry>,
    regions: Vec<Build>,
    requests: usize,
}
impl Job {
    fn run(
        mut self,
        input: Receiver<SourcePage>,
        recycle: mpsc::Sender<Vec<Quad>>,
        pool: &rayon::ThreadPool,
        tables: &Tables,
        history: &[History],
    ) -> Result<Option<PackedRegions>, Error> {
        let pages = pool.install(|| {
            input
                .into_iter()
                .par_bridge()
                .map(|p| {
                    let r = &self.regions[p.job];
                    let page = match p.input {
                        PageInput::Quads(quads) => {
                            let page = encode(&quads, r.key, tables, r.check.is_some());
                            let _ = recycle.send(quads);
                            page
                        }
                        PageInput::Tint(attrs) => Ok(super::terrain_packets::Page {
                            attrs,
                            geometry: Default::default(),
                            lamps: Vec::new(),
                            maps: Vec::new(),
                            opaque: true,
                        }),
                    };
                    page.map(|v| (p.job, p.piece, v))
                })
                .collect::<Result<Vec<_>, Error>>()
        })?;
        if pages.len() != self.requests {
            return Ok(None);
        }
        for (region, i, page) in pages {
            self.regions[region].pieces[i].1 = Piece::Fresh(Some(page));
        }
        let built = pool.install(|| {
            self.regions
                .into_par_iter()
                .map(|r| r.finish(history))
                .collect::<Result<Vec<_>, _>>()
        })?;
        let mut built = built.into_iter();
        let (slots, entries, _) = self.update.parts_mut();
        let mut patches = std::collections::BTreeMap::new();
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(built.len())
            .map_err(scene_cache::Error::from)?;
        let mut opacity_indices = Vec::new();
        for entry in self.entries {
            match entry {
                Entry::Reuse(i) => entries.push(scene_cache::Prepared::Reused(i)),
                Entry::Build => {
                    let output = built.next().expect("构造结果与候选同序");
                    let region = match output {
                        super::terrain_packets::Output::Geometry(mut r, moved) => {
                            if moved {
                                r.range = slots.take(r.range.len() as u32)?;
                                if r.range.end > self.limit {
                                    return Err(Error::Invalid("区域属性槽超限"));
                                }
                            }
                            r
                        }
                        super::terrain_packets::Output::Attributes(old, rows) => {
                            patches.insert(entries.len(), rows);
                            entries.push(scene_cache::Prepared::Reused(old));
                            continue;
                        }
                    };
                    pending.push((
                        entries.len(),
                        append_indices(&mut opacity_indices, &region)?,
                    ));
                    entries.push(scene_cache::Prepared::Built(region));
                }
            }
        }
        Ok(Some(PackedRegions {
            patches,
            update: self.update,
            pending,
            opacity_indices,
        }))
    }
}
struct Returned {
    elapsed: u64,
    encoded: u64,
    reused: u64,
    result: Result<Option<PackedRegions>, Error>,
    history: Vec<History>,
    tables: Tables,
    pool: rayon::ThreadPool,
}
struct Feed {
    generation: u64,
    top: super::AccelerationStructure,
    stamps: Stamps,
    requests: Vec<Request>,
    position: usize,
    sender: Option<SyncSender<SourcePage>>,
    held: Option<SourcePage>,
    recycle: Receiver<Vec<Quad>>,
    cancelled: bool,
}
impl Feed {
    fn matches(
        &self,
        gpu_generation: u64,
        top: super::AccelerationStructure,
        source: RenderView<'_>,
    ) -> bool {
        !self.cancelled
            && self.generation == gpu_generation
            && self.generation == source.generation
            && self.top == top
            && self.stamps.iter().copied().eq(source
                .regions
                .iter()
                .map(|r| (r.key, r.geometry_revision, r.content_revision)))
    }
    fn stop(&mut self) {
        self.cancelled = true;
        self.sender = None;
        self.held = None;
    }
    fn advance(&mut self, source: RenderView<'_>) -> Result<usize, Error> {
        let Some(sender) = &self.sender else {
            return Ok(0);
        };
        let started = Instant::now();
        let mut copied = 0usize;
        while let Some(r) = self.requests.get(self.position) {
            let p = if let Some(p) = self.held.take() {
                p
            } else {
                let mesh = source.regions[r.region].mesh;
                let input = if let Some(rgb) = r.tint {
                    let slab = mesh
                        .fragment_slice(r.slot, r.range.clone())
                        .ok_or(Error::Invalid("颜色交接范围失效"))?;
                    let mut attrs = Vec::new();
                    attrs
                        .try_reserve_exact(slab.len())
                        .map_err(scene_cache::Error::from)?;
                    attrs.extend(slab.iter().map(|q| {
                        let mut a = q.attributes();
                        a[2][..3].copy_from_slice(&rgb);
                        a
                    }));
                    copied += attrs.len() * scene_cache::QUAD_ATTRIBUTE_BYTES;
                    PageInput::Tint(attrs)
                } else {
                    let mut quads = self.recycle.try_recv().unwrap_or_default();
                    mesh.copy_fragment(r.slot, r.range.clone(), &mut quads)?;
                    copied += quads.len() * size_of::<Quad>();
                    PageInput::Quads(quads)
                };
                SourcePage {
                    job: r.job,
                    piece: r.piece,
                    input,
                }
            };
            match sender.try_send(p) {
                Ok(()) => self.position += 1,
                Err(TrySendError::Full(p)) => {
                    self.held = Some(p);
                    return Ok(copied);
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.sender = None;
                    return Ok(copied);
                }
            }
            if copied >= HANDOFF_BYTES || started.elapsed() >= HANDOFF_TIME {
                break;
            }
        }
        if self.position == self.requests.len() {
            self.sender = None;
        }
        Ok(copied)
    }
}
pub(super) struct CpuPackets {
    pub handoff_bytes: usize,
    pub encoded: u64,
    pub reused: u64,
    feed: Option<Feed>,
    worker: scene_cache::Producer<Returned>,
    pool: Option<rayon::ThreadPool>,
}
impl CpuPackets {
    pub fn new() -> Result<Self, Error> {
        let count = std::thread::available_parallelism().map_or(1, |c| c.get().min(16));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(count)
            .thread_name(|i| format!("prime-quad-{i}"))
            .build()?;
        Ok(Self {
            handoff_bytes: 0,
            encoded: 0,
            reused: 0,
            feed: None,
            worker: scene_cache::Producer::new("prime-terrain")?,
            pool: Some(pool),
        })
    }
    pub fn pending(&self) -> bool {
        self.feed.is_some()
    }
}
impl Drop for CpuPackets {
    fn drop(&mut self) {
        drop(self.feed.take());
    }
}
impl Gpu {
    fn restore_packets(&mut self, returned: Returned, accept: bool) -> Result<(), Error> {
        let feed = self.cpu_packets.feed.take().expect("后台批次持有源租约");
        self.cpu_packets.pool = Some(returned.pool);
        if self.resource_generation == feed.generation {
            self.material_coverage = returned.tables.coverage;
            self.light_sides = returned.tables.sides;
            self.light_areas = returned.tables.areas;
            self.light_weights = returned.tables.weights;
            self.opacity.indices = returned.tables.maps;
        }
        if self.scene.top == feed.top && self.resource_generation == feed.generation {
            for h in returned.history {
                let old = &mut self.region_cache.entries_mut()[h.index];
                old.resident = h.resident;
                old.lamps = h.lamps;
                old.opacity_slots = h.maps;
            }
        }
        if accept {
            self.cpu_packets.encoded = returned.encoded;
            self.cpu_packets.reused = returned.reused;
            self.cpu_ready = returned.result?;
        }
        Ok(())
    }
    pub(super) fn cancel_packets(&mut self) -> Result<(), Error> {
        self.cpu_ready = None;
        if let Some(feed) = &mut self.cpu_packets.feed {
            feed.stop();
        }
        if let Some(done) = self.cpu_packets.worker.wait()? {
            self.restore_packets(done, false)?;
        }
        Ok(())
    }
    pub(super) fn poll_packets(&mut self, source: RenderView<'_>) -> Result<u64, Error> {
        let Some(feed) = self.cpu_packets.feed.as_mut() else {
            return Ok(0);
        };
        let accept = feed.matches(self.resource_generation, self.scene.top, source);
        if accept {
            self.cpu_packets.handoff_bytes = feed.advance(source)?;
        } else {
            feed.stop();
        }
        if let Some(done) = self.cpu_packets.worker.poll()? {
            let elapsed = done.elapsed;
            self.restore_packets(done, accept)?;
            return Ok(elapsed);
        }
        Ok(0)
    }
    pub(super) fn start_packets(
        &mut self,
        source: RenderView<'_>,
        matches: &[Matched],
    ) -> Result<(), Error> {
        let mut update = self.region_cache.begin(source.regions.len())?;
        let (slots, _, ids) = update.parts_mut();
        let mut regions = Vec::new();
        let mut entries = Vec::new();
        let mut requests = Vec::new();
        let mut loan_indices = Vec::new();
        let mut stamps = Vec::new();
        loan_indices
            .try_reserve(source.regions.len())
            .map_err(scene_cache::Error::from)?;
        entries
            .try_reserve_exact(matches.len())
            .map_err(scene_cache::Error::from)?;
        regions
            .try_reserve(matches.len())
            .map_err(scene_cache::Error::from)?;
        stamps
            .try_reserve_exact(matches.len())
            .map_err(scene_cache::Error::from)?;
        for (index, (region, matched)) in source.regions.iter().zip(matches).enumerate() {
            stamps.push((
                region.key,
                region.geometry_revision,
                region.content_revision,
            ));
            if let Some(i) = matched.reuse
                && !matched.attributes_dirty
            {
                entries.push(Entry::Reuse(i));
                continue;
            }
            let count =
                u32::try_from(region.mesh.len()).map_err(|_| Error::Invalid("区域面数溢出"))?;
            let check = matched.reuse.map(|i| super::terrain_packets::Check {
                old: i,
                opaque: self.region_cache.entries()[i].opaque,
                tint: region.mesh.attribute_only(),
            });
            let range = if let Some(c) = &check {
                let range = self.region_cache.entries()[c.old].range.clone();
                if range.len() != count as usize {
                    return Err(Error::Invalid("同一几何版本的面数改变"));
                }
                range
            } else {
                slots.take(count)?
            };
            if range.end > self.attribute_slot_limit {
                return Err(Error::Invalid("区域超出设备属性槽容量"));
            }
            let old = self
                .region_cache
                .entries()
                .binary_search_by_key(&region.key, |v| v.key)
                .ok();
            let history = old.map(|i| {
                let loan = loan_indices.len();
                loan_indices.push(i);
                loan
            });
            let old = old.map(|i| {
                (
                    &self.region_cache.entries()[i],
                    self.attribute_revisions[i].1,
                )
            });
            let tint = check.as_ref().is_some_and(|c| c.tint);
            let mut region_job = Build {
                check,
                starts: Vec::new(),
                key: region.key,
                geometry: region.geometry_revision,
                content: region.content_revision,
                range,
                ids: ids.reserve_quads(count as usize)?,
                history,
                fragments: Vec::new(),
                pieces: Vec::new(),
            };
            let mut first = 0;
            for (slot, mesh) in region.mesh.fragments() {
                let stamp = region.mesh.fragment_revision(slot).expect("已枚举的片段");
                let len = mesh.quads.len();
                let reuse = old
                    .filter(|_| !tint)
                    .filter(|(v, current)| {
                        stamp != 0
                            && v.baked_content == *current
                            && region.mesh.fragment_tints(slot).is_empty()
                    })
                    .and_then(|(v, _)| {
                        v.fragments
                            .binary_search_by_key(&slot, |s| s.slot)
                            .ok()
                            .map(|i| v.fragments[i])
                    })
                    .filter(|s| s.stamp == stamp && s.len == len);
                let f = region_job.fragments.len();
                region_job
                    .fragments
                    .try_reserve(1)
                    .map_err(scene_cache::Error::from)?;
                region_job.fragments.push(FragmentSpan {
                    slot,
                    stamp,
                    first,
                    len,
                    opaque: reuse.is_none_or(|s| s.opaque),
                });
                if let Some(old) = reuse {
                    region_job
                        .starts
                        .try_reserve(1)
                        .map_err(scene_cache::Error::from)?;
                    region_job.starts.push(first);
                    region_job
                        .pieces
                        .try_reserve(1)
                        .map_err(scene_cache::Error::from)?;
                    region_job.pieces.push((
                        f,
                        Piece::Previous {
                            first: old.first,
                            len,
                        },
                    ));
                } else {
                    let pages = len.div_ceil(PAGE);
                    region_job
                        .pieces
                        .try_reserve(pages)
                        .map_err(scene_cache::Error::from)?;
                    requests
                        .try_reserve(pages)
                        .map_err(scene_cache::Error::from)?;
                    let add = |tint: Option<[u32; 3]>,
                               start: usize,
                               end: usize,
                               job: &mut Build,
                               requests: &mut Vec<Request>|
                     -> Result<(), Error> {
                        let n = (end - start).div_ceil(PAGE);
                        requests.try_reserve(n).map_err(scene_cache::Error::from)?;
                        job.starts
                            .try_reserve(n)
                            .map_err(scene_cache::Error::from)?;
                        job.pieces
                            .try_reserve(n)
                            .map_err(scene_cache::Error::from)?;
                        for from in (start..end).step_by(PAGE) {
                            job.starts.push(first + from);
                            requests.push(Request {
                                tint,
                                region: index,
                                slot,
                                range: from..(from + PAGE).min(end),
                                job: regions.len(),
                                piece: job.pieces.len(),
                            });
                            job.pieces.push((f, Piece::Fresh(None)));
                        }
                        Ok(())
                    };
                    if tint {
                        for p in region.mesh.fragment_tints(slot) {
                            add(
                                Some(
                                    [16, 8, 0].map(|s| {
                                        ((((p.argb() >> s) & 255) as f32) / 255.).to_bits()
                                    }),
                                ),
                                p.first(),
                                p.first() + p.count(),
                                &mut region_job,
                                &mut requests,
                            )?;
                        }
                    } else {
                        add(None, 0, len, &mut region_job, &mut requests)?;
                    }
                }
                first += len;
            }
            entries.push(Entry::Build);
            regions.push(region_job);
        }
        let mut history = Vec::new();
        history
            .try_reserve_exact(loan_indices.len())
            .map_err(scene_cache::Error::from)?;
        let pool = self.cpu_packets.pool.take().expect("一个在途批次独占池");
        let (sender, input) = mpsc::sync_channel(pool.current_num_threads() * 2);
        let (back, recycle) = mpsc::channel();
        let encoded = requests
            .iter()
            .filter(|r| regions[r.job].check.is_none())
            .map(|r| r.range.len() as u64)
            .sum();
        let reused = regions
            .iter()
            .flat_map(|r| &r.pieces)
            .map(|(_, p)| {
                if let Piece::Previous { len, .. } = p {
                    *len as u64
                } else {
                    0
                }
            })
            .sum();
        let job = Job {
            limit: self.attribute_slot_limit,
            update,
            entries,
            regions,
            requests: requests.len(),
        };
        let feed = Feed {
            generation: source.generation,
            top: self.scene.top,
            stamps,
            requests,
            position: 0,
            sender: Some(sender),
            held: None,
            recycle,
            cancelled: false,
        };
        for i in loan_indices {
            let old = &mut self.region_cache.entries_mut()[i];
            history.push(History {
                index: i,
                lamps: old.lamps.take(),
                resident: std::mem::take(&mut old.resident),
                maps: std::mem::take(&mut old.opacity_slots),
            });
        }
        let tables = Tables {
            sides: std::mem::take(&mut self.light_sides),
            areas: std::mem::take(&mut self.light_areas),
            weights: std::mem::take(&mut self.light_weights),
            coverage: std::mem::take(&mut self.material_coverage),
            maps: std::mem::take(&mut self.opacity.indices),
        };
        self.cpu_packets.worker.submit(move || {
            let started = Instant::now();
            let result = job.run(input, back, &pool, &tables, &history);
            Returned {
                result,
                history,
                tables,
                pool,
                encoded,
                reused,
                elapsed: super::elapsed_ns(started),
            }
        })?;
        self.cpu_packets.feed = Some(feed);
        self.cpu_packets.handoff_bytes = self
            .cpu_packets
            .feed
            .as_mut()
            .expect("已移交任务")
            .advance(source)?;
        Ok(())
    }
}
