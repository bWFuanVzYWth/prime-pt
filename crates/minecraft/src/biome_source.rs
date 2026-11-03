//! 26.3原版biome zoom、颜色映射及Simplex源计算；源字段只在边界进入。
use crate::{
    Error,
    biome::{Resolver, Sample},
    tint::{ColumnCache, Section},
    wire::Reader,
};
use std::{
    collections::{HashMap, HashSet},
    simd::{Simd, num::SimdUint},
};

pub(crate) struct Definitions {
    permutation: [u8; 256],
    offset: [f64; 2],
    maps: [Vec<u32>; 3],
}
impl Definitions {
    pub fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let mut permutation = [0; 256];
        let mut seen = [false; 256];
        for p in &mut permutation {
            let n = r.u32()?;
            if n > 255 || seen[n as usize] {
                return Err(Error::Input("biome noise permutation无效"));
            }
            seen[n as usize] = true;
            *p = n as u8;
        }
        let offset = [r.f64()?, r.f64()?];
        let mut maps = std::array::from_fn(|_| Vec::new());
        for map in &mut maps {
            let count = r.count(4)?;
            if count > 65536 {
                return Err(Error::Input("biome colormap超过65536"));
            }
            map.try_reserve_exact(count)?;
            for _ in 0..count {
                map.push(r.u32()?);
            }
        }
        Ok(Self {
            permutation,
            offset,
            maps,
        })
    }
    fn color(&self, temperature: f32, downfall: f32, map: usize) -> u32 {
        let t = f64::from(temperature.clamp(0., 1.));
        let humidity = f64::from(downfall.clamp(0., 1.)) * t;
        let index = ((1. - t) * 255.) as usize | (((1. - humidity) * 255.) as usize) << 8;
        self.maps[map]
            .get(index)
            .copied()
            .unwrap_or([0xffff00ff, 0xff48b518, 0xff5c3c32][map])
    }
    fn swamp(&self, x: i32, z: i32) -> u32 {
        // SimplexNoise.get先返回float，随后grass modifier按double阈值比较。
        let noise = f64::from(simplex(
            &self.permutation,
            f64::from(x) * 0.0225 + self.offset[0],
            f64::from(z) * 0.0225 + self.offset[1],
        ) as f32);
        if noise < -0.1 { 0xff4c763c } else { 0xff6a7039 }
    }
}
#[derive(Clone, Copy, Default)]
pub(crate) struct BiomeColor {
    colors: [u32; 4],
    swamp: bool,
}
impl BiomeColor {
    pub fn read(r: &mut Reader<'_>, d: &Definitions) -> Result<Self, Error> {
        let temperature = r.f32()?;
        let downfall = r.f32()?;
        let water = r.u32()?;
        let overrides = [r.u32()?, r.u32()?, r.u32()?];
        let flags = r.u32()?;
        let modifier = r.u32()?;
        if flags & !7 != 0 || modifier > 2 {
            return Err(Error::Input("biome源flags/modifier无效"));
        }
        let mut colors = std::array::from_fn(|i| {
            if i == 3 {
                water
            } else if flags & (1 << i) != 0 {
                overrides[i]
            } else {
                d.color(temperature, downfall, i)
            }
        });
        if modifier == 1 {
            colors[0] = 0xff000000 | (((colors[0] & 0xfefefe) + 0x28340a) >> 1);
        }
        Ok(Self {
            colors,
            swamp: modifier == 2,
        })
    }
    fn color(self, d: &Definitions, sample: Sample) -> u32 {
        if self.swamp && sample.resolver == Resolver::Grass {
            d.swamp(sample.position[0], sample.position[2])
        } else {
            self.colors[sample.resolver as usize - 1]
        }
    }
}
#[derive(Clone)]
struct Palette {
    valid: u64,
    colors: [BiomeColor; 64],
}
impl Default for Palette {
    fn default() -> Self {
        Self {
            valid: 0,
            colors: [BiomeColor::default(); 64],
        }
    }
}
#[derive(Default)]
pub(crate) struct Cache {
    palettes: ColumnCache<Box<Palette>>,
    zoom: ZoomCache,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub section: Section,
    pub mask: u64,
    tile: usize,
}
struct Tile {
    section: Section,
    palette: Box<Palette>,
    wanted: u64,
}
pub(crate) struct Plan {
    pub requests: Vec<Request>,
    tiles: Vec<Tile>,
    locations: Vec<usize>,
}
impl Cache {
    pub fn invalidate(&mut self, all: bool, columns: &HashSet<(i32, i32)>) {
        if all {
            self.palettes.clear();
        } else {
            for &column in columns {
                self.palettes.remove_column(column);
            }
        }
    }
    pub fn forget(&mut self, consumer: Section) {
        for section in consumer.halo() {
            self.palettes.remove(&section);
            self.zoom.pages.remove(&section);
        }
    }
    pub fn prepare(&mut self, seed: u64, samples: &[Sample]) -> Plan {
        let positions = self.zoom.resolve(seed, samples);
        let mut plan = Plan {
            requests: Vec::new(),
            tiles: Vec::new(),
            locations: Vec::with_capacity(samples.len()),
        };
        let mut indices = HashMap::new();
        let mut previous = None;
        for [x, y, z] in positions {
            let section = Section(x >> 2, y >> 2, z >> 2);
            let index = match previous {
                Some((key, index)) if key == section => index,
                _ => {
                    let index = *indices.entry(section).or_insert_with(|| {
                        let index = plan.tiles.len();
                        plan.tiles.push(Tile {
                            section,
                            palette: self.palettes.get(&section).cloned().unwrap_or_default(),
                            wanted: 0,
                        });
                        index
                    });
                    previous = Some((section, index));
                    index
                }
            };
            let local = ((x & 3) | (z & 3) << 2 | (y & 3) << 4) as usize;
            plan.tiles[index].wanted |= 1 << local;
            plan.locations.push(index * 64 + local);
        }
        for (tile, data) in plan.tiles.iter().enumerate() {
            let mask = data.wanted & !data.palette.valid;
            if mask != 0 {
                plan.requests.push(Request {
                    section: data.section,
                    mask,
                    tile,
                });
            }
        }
        plan
    }
    pub fn finish(
        &mut self,
        mut plan: Plan,
        ids: &[u32],
        palette: &[BiomeColor],
        samples: &[Sample],
        d: &Definitions,
    ) -> Vec<u32> {
        let mut ids = ids.iter();
        for request in &plan.requests {
            let tile = &mut plan.tiles[request.tile].palette;
            let mut bits = request.mask;
            while bits != 0 {
                let local = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                tile.colors[local] = palette[*ids.next().unwrap() as usize];
            }
            tile.valid |= request.mask;
        }
        let colors = plan
            .locations
            .iter()
            .zip(samples)
            .map(|(&location, sample)| {
                plan.tiles[location / 64].palette.colors[location & 63].color(d, *sample)
            })
            .collect();
        let mut changed = plan.requests.into_iter().map(|r| r.tile).peekable();
        for (index, tile) in plan.tiles.into_iter().enumerate() {
            if changed.peek() == Some(&index) {
                changed.next();
                self.palettes.insert(tile.section, tile.palette);
            }
        }
        colors
    }
}

/// Each quart cube has the same eight jittered corners for all 64 block positions.
/// Seed-only winners survive biome/colormap changes; consumer retirement bounds their storage.
#[derive(Default)]
struct ZoomCache {
    seed: Option<u64>,
    pages: HashMap<Section, Box<ZoomPage>>,
}
struct ZoomPage {
    cubes: [Option<Box<ZoomCube>>; 64],
}
impl Default for ZoomPage {
    fn default() -> Self {
        Self {
            cubes: std::array::from_fn(|_| None),
        }
    }
}
struct ZoomCube {
    jitter: [[f64; 8]; 3],
    winners: [u8; 64],
}
impl ZoomCube {
    fn new(seed: u64, base: [i32; 3]) -> Self {
        type Corners = Simd<u64, 8>;
        let next = |state: Corners, salt| {
            state
                * (state * Corners::splat(6364136223846793005)
                    + Corners::splat(1442695040888963407))
                + salt
        };
        let fiddle = |state: Corners| {
            // The masked value is at most 1023; narrow before conversion to avoid emulated u64鈫抐64.
            let value = ((state >> 24) & Corners::splat(1023))
                .cast::<u32>()
                .cast::<f64>();
            ((value / Simd::splat(1024.)) - Simd::splat(0.5)) * Simd::splat(0.9)
        };
        let quart: [_; 3] = std::array::from_fn(|axis| {
            Corners::from_array(std::array::from_fn(|corner| {
                (base[axis] + ((corner >> (2 - axis)) & 1) as i32) as i64 as u64
            }))
        });
        let mut state = Corners::splat(seed);
        for _ in 0..2 {
            for coordinate in quart {
                state = next(state, coordinate);
            }
        }
        let x = fiddle(state).to_array();
        state = next(state, Corners::splat(seed));
        let y = fiddle(state).to_array();
        state = next(state, Corners::splat(seed));
        Self {
            jitter: [x, y, fiddle(state).to_array()],
            winners: [8; 64],
        }
    }
    fn resolve(&mut self, fraction: [i32; 3]) -> u8 {
        let local = (fraction[0] | fraction[2] << 2 | fraction[1] << 4) as usize;
        if self.winners[local] < 8 {
            return self.winners[local];
        }
        let f = fraction.map(|v| f64::from(v) / 4.);
        type Corners = Simd<f64, 8>;
        let distance = |axis, delta| {
            (Corners::splat(f[axis]) - Corners::from_array(delta))
                + Corners::from_array(self.jitter[axis])
        };
        let x = distance(0, [0., 0., 0., 0., 1., 1., 1., 1.]);
        let y = distance(1, [0., 0., 1., 1., 0., 0., 1., 1.]);
        let z = distance(2, [0., 1., 0., 1., 0., 1., 0., 1.]);
        // Preserve vanilla's (z虏 + y虏) + x虏 order, without FMA. Lowest lane wins ties.
        let distance = z * z + y * y + x * x;
        let mut best = f64::INFINITY;
        let mut winner = 0;
        for (corner, distance) in distance.to_array().into_iter().enumerate() {
            if distance < best {
                best = distance;
                winner = corner as u8;
            }
        }
        self.winners[local] = winner;
        winner
    }
}
impl ZoomCache {
    fn resolve(&mut self, seed: u64, samples: &[Sample]) -> Vec<[i32; 3]> {
        if self.seed != Some(seed) {
            self.pages.clear();
            self.seed = Some(seed);
        }
        let mut indices = HashMap::new();
        let mut pages: Vec<(Section, Box<ZoomPage>)> = Vec::new();
        let mut previous = None;
        let mut positions = Vec::with_capacity(samples.len());
        for sample in samples {
            let shifted = sample.position.map(|v| v.wrapping_sub(2));
            let base = shifted.map(|v| v >> 2);
            let section = Section(shifted[0] >> 4, shifted[1] >> 4, shifted[2] >> 4);
            let index = match previous {
                Some((key, index)) if key == section => index,
                _ => {
                    let index = *indices.entry(section).or_insert_with(|| {
                        let index = pages.len();
                        pages.push((section, self.pages.remove(&section).unwrap_or_default()));
                        index
                    });
                    previous = Some((section, index));
                    index
                }
            };
            let local = ((base[0] & 3) | (base[2] & 3) << 2 | (base[1] & 3) << 4) as usize;
            let cube = pages[index].1.cubes[local]
                .get_or_insert_with(|| Box::new(ZoomCube::new(seed, base)));
            let winner = i32::from(cube.resolve(shifted.map(|v| v & 3)));
            positions.push([
                base[0] + (winner >> 2),
                base[1] + ((winner >> 1) & 1),
                base[2] + (winner & 1),
            ]);
        }
        self.pages.extend(pages);
        positions
    }
}
#[cfg(test)]
fn next(state: u64, salt: u64) -> u64 {
    state
        .wrapping_mul(
            state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        )
        .wrapping_add(salt)
}
#[cfg(test)]
fn fiddle(state: u64) -> f64 {
    ((((state >> 24) & 1023) as f64 / 1024.) - 0.5) * 0.9
}

fn simplex(permutation: &[u8; 256], x: f64, y: f64) -> f64 {
    const GRADIENT: [[f64; 2]; 12] = [
        [1., 1.],
        [-1., 1.],
        [1., -1.],
        [-1., -1.],
        [1., 0.],
        [-1., 0.],
        [1., 0.],
        [-1., 0.],
        [0., 1.],
        [0., -1.],
        [0., 1.],
        [0., -1.],
    ];
    let f2 = 0.5 * (3f64.sqrt() - 1.);
    let g2 = (3. - 3f64.sqrt()) / 6.;
    let skew = (x + y) * f2;
    let i = (x + skew).floor() as i32;
    let j = (y + skew).floor() as i32;
    let unskew = f64::from(i + j) * g2;
    let x0 = x - (f64::from(i) - unskew);
    let y0 = y - (f64::from(j) - unskew);
    let (di, dj) = if x0 > y0 { (1, 0) } else { (0, 1) };
    let p = |v: i32| i32::from(permutation[(v & 255) as usize]);
    let corner = |gi: i32, x: f64, y: f64| {
        let a = 0.5 - x * x - y * y - 0.;
        if a < 0. {
            0.
        } else {
            let a = a * a;
            let g = GRADIENT[gi as usize % 12];
            a * a * (g[0] * x + g[1] * y + 0.)
        }
    };
    let n0 = corner(p(i + p(j)), x0, y0);
    let n1 = corner(
        p(i + di + p(j + dj)),
        x0 - f64::from(di) + g2,
        y0 - f64::from(dj) + g2,
    );
    let n2 = corner(p(i + 1 + p(j + 1)), x0 - 1. + 2. * g2, y0 - 1. + 2. * g2);
    70. * (n0 + n1 + n2)
}

#[cfg(test)]
impl Cache {
    pub(crate) fn resident_keys(&self) -> Vec<Section> {
        self.palettes
            .keys()
            .chain(self.zoom.pages.keys().copied())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_zoom_matches_ordered_scalar_distances_and_ties() {
        for seed in [0, 1, 42, u64::MAX, 0x1357_2468_ffff_0000] {
            for base in [[0, 0, 0], [-1, -2, -3], [-1_000_000, 300, 2_000_000]] {
                let mut cube = ZoomCube::new(seed, base);
                for (corner, _) in cube.jitter[0].iter().enumerate() {
                    let quart: [_; 3] = std::array::from_fn(|axis| {
                        base[axis] + ((corner >> (2 - axis)) & 1) as i32
                    });
                    let mut state = seed;
                    for _ in 0..2 {
                        for coordinate in quart {
                            state = next(state, coordinate as i64 as u64);
                        }
                    }
                    for jitter in &cube.jitter {
                        assert_eq!(jitter[corner].to_bits(), fiddle(state).to_bits());
                        state = next(state, seed);
                    }
                }
                for y in 0..4 {
                    for z in 0..4 {
                        for x in 0..4 {
                            let f = [x, y, z].map(|v| f64::from(v) / 4.);
                            let mut best = f64::INFINITY;
                            let mut expected = 0;
                            for corner in 0..8 {
                                let x =
                                    (f[0] - ((corner >> 2) & 1) as f64) + cube.jitter[0][corner];
                                let y =
                                    (f[1] - ((corner >> 1) & 1) as f64) + cube.jitter[1][corner];
                                let z = (f[2] - (corner & 1) as f64) + cube.jitter[2][corner];
                                let distance = z * z + y * y + x * x;
                                if distance < best {
                                    best = distance;
                                    expected = corner as u8;
                                }
                            }
                            assert_eq!(cube.resolve([x, y, z]), expected);
                            assert_eq!(cube.resolve([x, y, z]), expected);
                        }
                    }
                }
            }
        }
        let mut tie = ZoomCube {
            jitter: [[0.; 8]; 3],
            winners: [8; 64],
        };
        assert_eq!(tie.resolve([2, 2, 2]), 0);
        assert_eq!(tie.resolve([2, 2, 3]), 1);
        assert_eq!(tie.resolve([3, 2, 2]), 4);
    }
}

#[cfg(test)]
mod java_fixture {
    use super::*;
    #[test]
    #[ignore = "先执行Gradle tintFixture；离屏真实26.3数学oracle"]
    fn actual_26_3_zoom_colors_and_source_recipes_match() {
        let path = std::env::var_os("PRIME_TINT_FIXTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/java-tint.bin")
            });
        let bytes = std::fs::read(path).unwrap();
        let mut r = Reader::new(&bytes).unwrap();
        assert_eq!(r.u32().unwrap(), 0x314d4254);
        assert_eq!(r.u32().unwrap(), 263);
        let definitions = Definitions::read(&mut r).unwrap();
        let mut zoom = ZoomCache::default();
        for _ in 0..r.count(32).unwrap() {
            let seed = r.u64().unwrap();
            let position = [r.i32().unwrap(), r.i32().unwrap(), r.i32().unwrap()];
            let expected = [r.i32().unwrap(), r.i32().unwrap(), r.i32().unwrap()];
            assert_eq!(
                zoom.resolve(
                    seed,
                    &[Sample {
                        position,
                        resolver: Resolver::Grass
                    }]
                ),
                [expected],
                "seed {seed} position {position:?}"
            );
        }
        for _ in 0..r.count(36).unwrap() {
            let biome = BiomeColor::read(&mut r, &definitions).unwrap();
            for _ in 0..r.count(24).unwrap() {
                let position = [r.i32().unwrap(), 0, r.i32().unwrap()];
                for resolver in [
                    Resolver::Grass,
                    Resolver::Foliage,
                    Resolver::DryFoliage,
                    Resolver::Water,
                ] {
                    assert_eq!(
                        biome.color(&definitions, Sample { position, resolver }),
                        r.u32().unwrap(),
                        "{resolver:?} {position:?}"
                    );
                }
            }
        }
        let count = r.count(1).unwrap();
        let resources = crate::tint::TintResources::decode(r.take(count).unwrap()).unwrap();
        for _ in 0..r.count(16).unwrap() {
            let state = r.u32().unwrap();
            let slot = r.i32().unwrap();
            let custom = r.u32().unwrap();
            let expected = r.u32().unwrap();
            assert_eq!(
                resources.fixture_color(state, slot),
                if custom != 0 { None } else { Some(expected) },
                "state {state}, slot {slot}"
            );
        }
        r.finish().unwrap();
    }
}
