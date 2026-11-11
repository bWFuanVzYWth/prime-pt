//! 纯 CPU 包内核。分片共享不可变的世代查找表，各自产出面流。区域拼接只重定位已有的编码。
use super::lighting::{Emitters, Record, record_for};
use super::{Error, RegionGpu, opacity, scene_cache, vulkan};
use prime_scene::Quad;
use scene_cache::{PackedQuads, QuadEncoder, QuadIds, ResidentQuads};
use std::{collections::HashMap, ops::Range};

pub(super) struct Tables {
    pub coverage: Vec<bool>,
    pub sides: Vec<u32>,
    pub areas: Vec<[u32; 4]>,
    pub weights: Vec<f32>,
    pub maps: HashMap<opacity::Key, [u32; 2]>,
}

#[derive(Clone, Copy)]
pub(super) struct FragmentSpan {
    pub slot: usize,
    pub stamp: u64,
    pub first: usize,
    pub len: usize,
    pub opaque: bool,
}
pub(super) struct History {
    pub index: usize,
    pub resident: ResidentQuads,
    pub lamps: Option<Vec<Record>>,
    pub maps: Vec<(u32, [u32; 2])>,
}
pub(super) struct Page {
    pub attrs: Vec<[[u32; 4]; 3]>,
    pub lamps: Vec<Record>,
    pub geometry: PackedQuads,
    pub maps: Vec<(u32, [u32; 2])>,
    pub opaque: bool,
}
pub(super) fn encode(
    source: &[Quad],
    key: [i32; 3],
    tables: &Tables,
    checked: bool,
) -> Result<Page, Error> {
    let mut encoder = QuadEncoder::with_capacity(if checked { 0 } else { source.len() })?;
    let mut attrs = Vec::new();
    if checked {
        attrs
            .try_reserve_exact(source.len())
            .map_err(scene_cache::Error::from)?;
    }
    let mut maps = Vec::new();
    let mut opaque = true;
    let mut lamps = Vec::new();
    let emitters = Emitters {
        sides: &tables.sides,
        areas: &tables.areas,
        weights: &tables.weights,
    };
    for (i, q) in source.iter().enumerate() {
        if checked {
            attrs.push(q.attributes());
            if !q.valid() {
                return Err(Error::Invalid("quad 含非有限值"));
            }
        }
        let material = q.material_id().index();
        let coverage = *tables
            .coverage
            .get(material as usize)
            .ok_or(Error::Invalid("quad 材质超出当前世代"))?;
        let uv = q.uv();
        if !checked
            && !uv
                .iter()
                .flatten()
                .chain(q.tint().iter())
                .all(|v| v.is_finite())
        {
            return Err(Error::Invalid("quad 属性非有限"));
        }
        opaque &= !coverage;
        if tables.sides.get(material as usize).is_some_and(|&s| s != 0)
            && let Some(record) = record_for(q, key, i, emitters)?
        {
            lamps.try_reserve(1).map_err(scene_cache::Error::from)?;
            lamps.push(record);
        }
        if !checked {
            encoder.push(q.positions(), 0, q.attributes())?;
        }
        if coverage
            && !tables.maps.is_empty()
            && let Some(&pair) = tables
                .maps
                .get(&(material, uv.map(|p| p.map(f32::to_bits))))
        {
            maps.try_reserve(1).map_err(scene_cache::Error::from)?;
            maps.push((i as u32, pair));
        }
    }
    Ok(Page {
        attrs,
        geometry: encoder.finish(),
        maps,
        opaque,
        lamps,
    })
}
fn records_in<T>(rows: &[T], first: usize, len: usize, index: impl Fn(&T) -> u32) -> &[T] {
    let a = rows.partition_point(|r| (index(r) as usize) < first);
    let b = a + rows[a..].partition_point(|r| (index(r) as usize) < first + len);
    &rows[a..b]
}

// 页与区域缓存均按源序排列；灯的属性槽在交给光源注册表前始终相对当前片段。
fn append_lamps(
    out: &mut Vec<Record>,
    rows: &[Record],
    origin: usize,
    first: usize,
) -> Result<(), Error> {
    out.try_reserve(rows.len())
        .map_err(scene_cache::Error::from)?;
    out.extend(rows.iter().copied().map(|mut record| {
        record.meta[3] = (first + (record.meta[3] as usize - origin)) as u32;
        record
    }));
    Ok(())
}

pub(super) enum Piece {
    Previous { first: usize, len: usize },
    Fresh(Option<Page>),
}
pub(super) struct Check {
    pub old: usize,
    pub opaque: bool,
    pub tint: bool,
}
pub(super) struct AttributeRows {
    pub attributes: Vec<[[u32; 4]; 3]>,
    pub ranges: Vec<(usize, Range<usize>)>,
    pub lamps: Option<Vec<Record>>,
}
pub(super) enum Output {
    Geometry(RegionGpu, bool),
    Attributes(usize, AttributeRows),
}
pub(super) struct Build {
    pub check: Option<Check>,
    pub starts: Vec<usize>,
    pub key: [i32; 3],
    pub geometry: u64,
    pub content: u64,
    pub range: Range<u32>,
    pub ids: QuadIds,
    pub history: Option<usize>,
    pub fragments: Vec<FragmentSpan>,
    pub pieces: Vec<(usize, Piece)>,
}
impl Build {
    pub fn finish(mut self, histories: &[History]) -> Result<Output, Error> {
        let old = self.history.map(|i| &histories[i]);
        if let Some(check) = &self.check {
            let old = old.expect("复用候选持有原始面记录");
            let mut maps = Vec::new();
            let mut opaque = true;
            for ((f, piece), &first) in self.pieces.iter().zip(&self.starts) {
                let (records, origin, solid) = match piece {
                    Piece::Previous { first: prior, len } => (
                        records_in(&old.maps, *prior, *len, |r| r.0),
                        *prior,
                        self.fragments[*f].opaque,
                    ),
                    Piece::Fresh(Some(page)) => (page.maps.as_slice(), 0, page.opaque),
                    Piece::Fresh(None) => unreachable!("属性页完整"),
                };
                maps.try_reserve(records.len())
                    .map_err(scene_cache::Error::from)?;
                maps.extend(
                    records
                        .iter()
                        .map(|&(i, p)| ((first + i as usize - origin) as u32, p)),
                );
                opaque &= solid;
            }
            if check.tint || ((!check.opaque || opaque) && maps == old.maps) {
                let mut rows = AttributeRows {
                    attributes: Vec::new(),
                    ranges: Vec::new(),
                    lamps: (!check.tint).then(Vec::new),
                };
                for ((_, piece), first) in self.pieces.into_iter().zip(self.starts) {
                    let page = match piece {
                        Piece::Previous { first: prior, len } => {
                            if let (Some(out), Some(previous)) = (&mut rows.lamps, &old.lamps) {
                                let src = records_in(previous, prior, len, |r| r.meta[3]);
                                append_lamps(out, src, prior, first)?;
                            }
                            continue;
                        }
                        Piece::Fresh(Some(page)) => page,
                        Piece::Fresh(None) => unreachable!("属性页完整"),
                    };
                    super::attribute_update::pack_patch(
                        &self.range,
                        first,
                        page.attrs.into_iter(),
                        &mut rows.attributes,
                        &mut rows.ranges,
                    )?;
                    if let Some(lamps) = &mut rows.lamps {
                        append_lamps(lamps, &page.lamps, 0, first)?;
                    }
                }
                return Ok(Output::Attributes(check.old, rows));
            }
            for ((_, piece), &first) in self.pieces.iter_mut().zip(&self.starts) {
                let page = match piece {
                    Piece::Previous { .. } => continue,
                    Piece::Fresh(Some(page)) => page,
                    Piece::Fresh(None) => unreachable!("全部属性页已交还"),
                };
                let mut encoder = QuadEncoder::with_capacity(page.attrs.len())?;
                for (i, attr) in page.attrs.drain(..).enumerate() {
                    encoder.rebind(&old.resident, first + i, attr)?;
                }
                page.geometry = encoder.finish();
            }
        }
        let single = self.pieces.len() == 1 && matches!(self.pieces[0].1, Piece::Fresh(_));
        let mut encoder = QuadEncoder::with_capacity(if single { 0 } else { self.range.len() })?;
        let mut maps = Vec::new();
        let mut base = 0usize;
        let mut lamps = Vec::new();
        let mut direct = None;
        for (fragment, piece) in self.pieces {
            let desc = &mut self.fragments[fragment];
            match piece {
                Piece::Previous { first, len } => {
                    let old = old.expect("已发布分片持有历史");
                    encoder.extend_resident(&old.resident, first..first + len)?;
                    let prior_maps = records_in(&old.maps, first, len, |r| r.0);
                    maps.try_reserve(prior_maps.len())
                        .map_err(scene_cache::Error::from)?;
                    maps.extend(
                        prior_maps
                            .iter()
                            .map(|&(i, p)| ((base + (i as usize - first)) as u32, p)),
                    );
                    if let Some(records) = &old.lamps {
                        let src = records_in(records, first, len, |r| r.meta[3]);
                        append_lamps(&mut lamps, src, first, base)?;
                    }
                    base += len;
                }
                Piece::Fresh(page) => {
                    let page = page.expect("完整批中每个页面都已返回");
                    let n = page.geometry.headers().len();
                    if single {
                        direct = Some(page.geometry);
                    } else {
                        encoder.extend_packed(&page.geometry, 0..n)?;
                    }
                    maps.try_reserve(page.maps.len())
                        .map_err(scene_cache::Error::from)?;
                    maps.extend(
                        page.maps
                            .into_iter()
                            .map(|(i, p)| ((base + i as usize) as u32, p)),
                    );
                    append_lamps(&mut lamps, &page.lamps, 0, base)?;
                    desc.opaque &= page.opaque;
                    base += n;
                }
            }
        }
        let resident = ResidentQuads::prepare_reserved(
            direct.unwrap_or_else(|| encoder.finish()),
            old.map(|h| &h.resident),
            self.ids,
        )?;
        Ok(Output::Geometry(
            RegionGpu {
                lamps: Some(lamps),
                key: self.key,
                revision: self.geometry,
                baked_content: self.content,
                opaque: self.fragments.iter().all(|s| s.opaque),
                opacity_slots: maps,
                fragments: self.fragments,
                range: self.range,
                blas: None,
                resident,
            },
            self.check.is_some(),
        ))
    }
}

pub(super) fn append_indices(
    result: &mut Vec<u32>,
    region: &RegionGpu,
) -> Result<Range<usize>, Error> {
    let first = result.len();
    if !region.opacity_slots.is_empty() {
        let n = region.range.len() * 2;
        result.try_reserve(n).map_err(scene_cache::Error::from)?;
        result.resize(first + n, vulkan::OPACITY_UNKNOWN);
        for &(i, pair) in &region.opacity_slots {
            let j = first + i as usize * 2;
            result[j..j + 2].copy_from_slice(&pair);
        }
    }
    Ok(first..result.len())
}
