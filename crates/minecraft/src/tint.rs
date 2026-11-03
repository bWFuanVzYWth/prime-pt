//! 原版tint的资源recipe、世界源缓存与显式批量续接。
//!
//! 首次/重载由`PTR1`准备完整tint资源；世界进入由`PBW1`提供seed及完整biome调色板。
//! 运行期只请求缺失quart身份，Rust执行zoom、Simplex、颜色表和整数box blend。
//! 未知自定义函数只通过独立宿主请求回填，不在纯编译内核中调用宿主。
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
};
static OWNER_IDS: AtomicU64 = AtomicU64::new(1);
fn owner_id() -> Result<u64, Error> {
    OWNER_IDS
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .map_err(|_| Error::Input("tint owner身份耗尽"))
}
pub(crate) use crate::tint_cache::{ColumnCache, Section};
use crate::{Error, TintRequest, biome, biome_source, wire::Reader};

/// 一个资源世代唯一的原版tint定义。无Java引用，提交由Catalog所有者完成。
pub struct TintResources {
    owner: u64,
    generation: u64,
    definitions: biome_source::Definitions,
    recipes: HashMap<(u32, i32), Option<biome::Recipe>>,
}
impl TintResources {
    /// 校验并拥有完整PTR1，失败不修改已发布资源。
    ///
    /// 包含263版本、非零世代、256项noise permutation、两项f64 offset、三个不超过
    /// 65536项的颜色表，以及唯一`(state,slot,kind,value)`recipe行。
    /// kind0/8为ARGB常量，1/2/3/4为草/叶/干叶/水，5为草且value0/1选择当前位置/下方，
    /// 6为红石power0..15，7为stem age0..7；kind9/value0保留宿主回退。
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes)?;
        if r.u32()? != 0x31525450 || r.u32()? != 263 {
            return Err(Error::Input("需要26.3 PTR1 tint资源"));
        }
        let generation = r.u64()?;
        if generation == 0 {
            return Err(Error::Input("tint资源世代为零"));
        }
        let definitions = biome_source::Definitions::read(&mut r)?;
        let count = r.count(16)?;
        let mut recipes = HashMap::new();
        recipes.try_reserve(count)?;
        for _ in 0..count {
            let state = r.u32()?;
            let slot = r.i32()?;
            let kind = r.u32()?;
            let value = r.u32()?;
            if slot < -1 {
                return Err(Error::Input("tint slot小于-1"));
            }
            let recipe = if kind == 9 && value == 0 {
                None
            } else {
                Some(biome::Recipe::read(kind, value)?)
            };
            if recipes.insert((state, slot), recipe).is_some() {
                return Err(Error::Input("tint recipe身份重复"));
            }
        }
        r.finish()?;
        Ok(Self {
            owner: owner_id()?,
            generation,
            definitions,
            recipes,
        })
    }
    #[cfg(test)]
    pub(crate) fn fixture_color(&self, state: u32, slot: i32) -> Option<u32> {
        match self.recipes.get(&(state, slot)) {
            Some(Some(biome::Recipe::Color(c))) => Some(*c),
            _ => None,
        }
    }
    /// 资源身份，必须与方块、材质与世界tint源的世代一致。
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// 运行期缺失原始quart的批量请求，C布局24字节。
///
/// `section*4`为quart原点；mask位序`x|z<<2|y<<4`，每轴局部范围0..4。
/// Java只读取`BiomeManager.getNoiseBiomeAtQuart`，不执行最终tint求值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct BiomeRequest {
    /// 16m section坐标，即4³ quart页。
    pub section: [i32; 3],
    /// 必须为零。
    pub reserved: u32,
    /// 需要源身份的quart集合，按低位到高位应答。
    pub mask: u64,
}

/// 一次显式tint续接；内部plan独占，未完成或丢弃不会发布颜色结果。
///
/// 同一owner的新plan或依赖失效会使旧plan不可完成。宿主与biome应答均完整校验后才写缓存。
pub struct TintPlan {
    owner: u64,
    serial: u64,
    generation: u64,
    output: Vec<(bool, usize)>,
    host: Vec<TintRequest>,
    requests: Vec<BiomeRequest>,
    blend: biome::Plan,
    source: biome_source::Plan,
}
impl TintPlan {
    /// 仅实际自定义source，顺序与finish的host_colors一致。
    pub fn host_requests(&self) -> &[TintRequest] {
        &self.host
    }
    /// 尚未缓存的quart身份，顺序与finish的ids一致。
    pub fn biome_requests(&self) -> &[BiomeRequest] {
        &self.requests
    }
    /// quart原始身份应答的准确数量。
    pub fn biome_sample_count(&self) -> usize {
        self.requests
            .iter()
            .map(|r| r.mask.count_ones() as usize)
            .sum()
    }
    /// 本批需要zoom/color计算的未缓存block位置数量，不含已缓存box结果。
    pub fn source_samples(&self) -> usize {
        self.blend.samples.len()
    }
    /// 本批命中的最终box结果数量。
    pub const fn cached_colors(&self) -> usize {
        self.blend.hits
    }
}

/// 单个真实世界的tint所有者；不共享世界数据，无线程池或隐藏调度。
///
/// 拥有一次world捕获的seed/biome palette与按列页缓存；资源重载和世界切换必须以decode
/// 建立新owner。局部群系变化清理对应quart列及zoom/filter依赖，移出section显式回收页。
pub struct TintWorld {
    owner: u64,
    resource: u64,
    generation: u64,
    seed: u64,
    palette: Vec<biome_source::BiomeColor>,
    blend: biome::Cache,
    source: biome_source::Cache,
    serial: u64,
}
impl TintWorld {
    /// 校验PBW1完整世界源。每项32字节为temperature/downfall、水色、三override、flags/modifier。
    /// 调色板行号为运行期quart应答身份；非法源失败不改变当前世界。
    pub fn decode(bytes: &[u8], resources: &TintResources) -> Result<Self, Error> {
        let mut r = Reader::new(bytes)?;
        if r.u32()? != 0x31574250 || r.u32()? != 263 {
            return Err(Error::Input("需要26.3 PBW1 biome源"));
        }
        let generation = r.u64()?;
        if generation != resources.generation {
            return Err(Error::Input("biome源资源世代不一致"));
        }
        let seed = r.u64()?;
        let count = r.count(32)?;
        if count == 0 || count > 65536 {
            return Err(Error::Input("biome palette需要1..=65536项"));
        }
        let mut palette = Vec::new();
        palette.try_reserve_exact(count)?;
        for _ in 0..count {
            palette.push(biome_source::BiomeColor::read(
                &mut r,
                &resources.definitions,
            )?);
        }
        r.finish()?;
        let owner = owner_id()?;
        Ok(Self {
            owner,
            resource: resources.owner,
            generation,
            seed,
            palette,
            blend: Default::default(),
            source: Default::default(),
            serial: 0,
        })
    }
    /// 对同世代源建立纯Rust批量计划；radius支持原版0..=7，位置限制在世界±32000000内。
    /// 缺少已准备recipe显式失败，不临时调用宿主或追加资源。
    /// owner计划序号耗尽后返回错误；调用者以完整世界源建立新owner恢复，不复用饱和值身份。
    pub fn plan(
        &mut self,
        requests: &[TintRequest],
        resources: &TintResources,
        radius: u32,
    ) -> Result<TintPlan, Error> {
        if radius > 7 || self.generation != resources.generation || self.resource != resources.owner
        {
            return Err(Error::Input("tint半径或资源世代无效"));
        }
        let mut native = Vec::new();
        let mut recipes = Vec::new();
        let mut host = Vec::new();
        let mut output = Vec::new();
        native.try_reserve_exact(requests.len())?;
        recipes.try_reserve_exact(requests.len())?;
        output.try_reserve_exact(requests.len())?;
        for request in requests {
            if request
                .position
                .iter()
                .any(|v| !(-32000000..=32000000).contains(v))
            {
                return Err(Error::Input("tint位置超出世界范围"));
            }
            let recipe = resources
                .recipes
                .get(&(request.state, request.slot))
                .ok_or(Error::Input("tint引用未准备的source身份"))?;
            if let Some(recipe) = recipe {
                output.push((false, native.len()));
                native.push(*request);
                recipes.push(*recipe);
            } else {
                output.push((true, host.len()));
                host.try_reserve(1)?;
                host.push(*request);
            }
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(Error::Input("tint plan序号耗尽"))?;
        let blend = self
            .blend
            .prepare(native.into_iter(), &recipes, radius as i32);
        let source = self.source.prepare(self.seed, &blend.samples);
        let requests = source
            .requests
            .iter()
            .map(|r| BiomeRequest {
                section: [r.section.0, r.section.1, r.section.2],
                reserved: 0,
                mask: r.mask,
            })
            .collect();
        self.serial = serial;
        Ok(TintPlan {
            owner: self.owner,
            serial,
            generation: self.generation,
            output,
            host,
            requests,
            blend,
            source,
        })
    }
    /// 完整验证两类应答，再计算并发布缓存；输出与原始TintRequest严格同序。
    /// 错误或旧plan不改缓存，调用者可保留旧mesh并重新plan。
    pub fn finish(
        &mut self,
        plan: TintPlan,
        resources: &TintResources,
        ids: &[u32],
        host_colors: &[u32],
    ) -> Result<Vec<u32>, Error> {
        let count: usize = plan
            .requests
            .iter()
            .map(|r| r.mask.count_ones() as usize)
            .sum();
        if plan.owner != self.owner
            || plan.serial != self.serial
            || plan.generation != self.generation
            || resources.generation != self.generation
            || self.resource != resources.owner
        {
            return Err(Error::Input("tint plan已失效"));
        }
        if ids.len() != count
            || ids.iter().any(|&id| id as usize >= self.palette.len())
            || host_colors.len() != plan.host.len()
        {
            return Err(Error::Input("tint源应答长度或身份无效"));
        }
        let next_serial = self
            .serial
            .checked_add(1)
            .ok_or(Error::Input("tint plan序号耗尽"))?;
        let mut output = Vec::new();
        output.try_reserve_exact(plan.output.len())?;
        let samples = self.source.finish(
            plan.source,
            ids,
            &self.palette,
            &plan.blend.samples,
            &resources.definitions,
        );
        let colors = self.blend.finish(plan.blend, &samples);
        for (host, index) in plan.output {
            output.push(if host {
                host_colors[index]
            } else {
                colors[index]
            });
        }
        // 一张plan只允许提交一次；所有内部调用后续均无可恢复失败。
        self.serial = next_serial;
        Ok(output)
    }
    /// 依赖变化。all清所有颜色页；局部columns只清实际变化列及zoom/filter依赖。
    /// 世界seed及一次完整palette不变；palette字段本身改变需decode新的PBW1。
    pub fn invalidate(&mut self, all: bool, columns: &HashSet<(i32, i32)>) {
        self.blend.invalidate(all, columns);
        self.source.invalidate(all, columns);
        self.serial = self.serial.saturating_add(1);
    }
    /// 消费section退出时回收其可能引用的页；邻近存活消费者可按需重算共享halo。
    pub fn forget_section(&mut self, key: [i32; 3]) {
        let section = Section(key[0], key[1], key[2]);
        self.blend.forget(section);
        self.source.forget(section);
        self.serial = self.serial.saturating_add(1);
    }
}

#[cfg(any(test, feature = "fixtures"))]
#[path = "tint_fixtures.rs"]
pub mod fixtures;
#[cfg(test)]
#[path = "tint_tests.rs"]
mod tests;
