//! 世界顶层以动态发光面与静态子树为叶。设备直接消费姿态计算后的表面，独占发光目录和拓扑。
use super::*;
use vulkan::native::LightWorld;

const TILE: u32 = 128;
const TILE_ARGS: u64 = 4;
const MERGE_ARGS: u64 = 5;
const POWER_ARGS: u64 = 22;
const BRANCH_ARGS: u64 = 26;

struct Build {
    sources: (Buffer, LightWorld),
    config: [u32; 8],
    collect: ComputePipeline,
    schedule: ComputePipeline,
    sort: ComputePipeline,
    power: [ComputePipeline; 4],
    radix: ComputePipeline,
    merges: Vec<ComputePipeline>,
}
pub(super) struct Lights {
    empty: Buffer,
    config: Buffer,
    schedule: Buffer,
    nodes: Buffer,
    capacity: u32,
    surfaces: u32,
    build: Option<Build>,
    programs: [ComputePipeline; 9],
    widths: Vec<Buffer>,
}
impl Lights {
    pub fn new(ctx: &mut Context, placeholder: Buffer, world: LightWorld) -> Result<Self, Error> {
        let empty = Buffer::upload(ctx, &[0; 16])?;
        let config = Buffer::mapped_device_local(ctx, 32)?;
        let schedule = Buffer::new(ctx, (BRANCH_ARGS + 1) * 16, false)?;
        let bindings = Bindings::new(
            ctx,
            &[
                Binding::StorageBuffer(config),
                Binding::StorageBuffer(placeholder),
                Binding::StorageBuffer(placeholder),
                Binding::LightWorld(world),
                Binding::StorageBuffer(schedule),
                Binding::StorageBuffer(empty),
            ],
        )?;
        let mut build = |entry| ComputePipeline::new(ctx, bindings, entry);
        let programs = [
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_collect.spv")),
                c"feature_lights_collect",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_schedule.spv")),
                c"feature_lights_schedule",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_sort.spv")),
                c"feature_lights_sort",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_1.spv")),
                c"feature_lights_1",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_64.spv")),
                c"feature_lights_64",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_4096.spv")),
                c"feature_lights_4096",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_262144.spv")),
                c"feature_lights_262144",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_radix.spv")),
                c"feature_lights_radix",
            ))?,
            build(shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_lights_merge.spv")),
                c"feature_lights_merge",
            ))?,
        ];
        Ok(Self {
            empty,
            config,
            schedule,
            nodes: empty,
            capacity: 0,
            surfaces: 0,
            build: None,
            programs,
            widths: Vec::new(),
        })
    }
    pub const fn empty(&self) -> Buffer {
        self.empty
    }
    pub const fn active(&self) -> bool {
        self.build.is_some()
    }
    pub const fn tree(&self) -> Buffer {
        if self.active() {
            self.nodes
        } else {
            self.empty
        }
    }
    pub fn reset(&mut self) {
        self.build = None;
        self.nodes = self.empty;
        self.capacity = 0;
        self.surfaces = 0;
    }
    pub fn set_surface_count(&mut self, count: u32) {
        self.surfaces = count;
    }
    pub fn prepare(
        &mut self,
        ctx: &mut Context,
        surfaces: Buffer,
        world: LightWorld,
        anchor: [f64; 3],
    ) -> Result<bool, Error> {
        if self.surfaces == 0 {
            return Ok(self.build.take().is_some());
        }
        let regions = u32::try_from(ctx.light_regions(world)?.len())
            .map_err(|_| Error::Invalid("静态灯区域过多"))?;
        let upper = self
            .surfaces
            .checked_add(regions)
            .filter(|&n| n <= 1 << 24)
            .ok_or(Error::Invalid("世界灯树候选超过 24 位选择域"))?;
        let leaves = upper.next_power_of_two().max(self.capacity);
        let origin = anchor.map(|x| (x / 64.) as i32 as u32);
        let config = [
            self.surfaces,
            regions,
            leaves,
            0,
            origin[0],
            origin[1],
            origin[2],
            0,
        ];
        let grow = leaves > self.capacity;
        if let Some(active) = &mut self.build {
            if !grow && active.sources == (surfaces, world) {
                if active.config != config {
                    self.config.write(ctx, 0, bytemuck::cast_slice(&config))?;
                    active.config = config;
                }
                return Ok(false);
            }
        }
        let nodes = if grow {
            Buffer::new(ctx, u64::from(leaves) * 9 * 16, false)?
        } else {
            self.nodes
        };
        let mut layout = [
            Binding::StorageBuffer(self.config),
            Binding::StorageBuffer(surfaces),
            Binding::StorageBuffer(nodes),
            Binding::LightWorld(world),
            Binding::StorageBuffer(self.schedule),
            Binding::StorageBuffer(self.empty),
        ];
        let bindings = Bindings::new(ctx, &layout)?;
        let [
            collect,
            schedule,
            sort,
            reduce_1,
            reduce_64,
            reduce_4096,
            reduce_262144,
            radix,
            merge,
        ] = self.programs;
        let mut bind = |kernel: ComputePipeline| kernel.rebind(ctx, bindings);
        let mut next = Build {
            sources: (surfaces, world),
            config,
            collect: bind(collect)?,
            schedule: bind(schedule)?,
            sort: bind(sort)?,
            power: [
                bind(reduce_1)?,
                bind(reduce_64)?,
                bind(reduce_4096)?,
                bind(reduce_262144)?,
            ],
            radix: bind(radix)?,
            merges: Vec::new(),
        };
        let passes = leaves.ilog2().saturating_sub(TILE.ilog2()) as usize;
        while self.widths.len() < passes {
            let width = TILE << self.widths.len();
            self.widths
                .push(Buffer::upload(ctx, bytemuck::bytes_of(&width))?);
        }
        for &width in &self.widths[..passes] {
            layout[5] = Binding::StorageBuffer(width);
            let binding = Bindings::new(ctx, &layout)?;
            next.merges.push(merge.rebind(ctx, binding)?);
        }
        if self
            .build
            .as_ref()
            .is_none_or(|active| active.config != config)
        {
            self.config.write(ctx, 0, bytemuck::cast_slice(&config))?;
        }
        self.nodes = nodes;
        self.capacity = leaves;
        self.build = Some(next);
        Ok(true)
    }
    fn indirect(
        &self,
        commands: &mut Commands,
        kernel: ComputePipeline,
        row: u64,
    ) -> Result<(), Error> {
        // SAFETY: schedule 着色器在前序屏障前写完全部参数；每维最多 65535 组，空阶段为零组。
        unsafe {
            commands.dispatch_indirect(kernel, self.schedule, row * 16)?;
        }
        Ok(())
    }
    pub fn record_sort(&self, commands: &mut Commands) -> Result<(), Error> {
        let Some(build) = &self.build else {
            return Ok(());
        };
        commands.barrier();
        commands.clear_buffer(self.schedule)?;
        commands.barrier();
        let groups = (build.config[0] + build.config[1]).div_ceil(TILE);
        commands.dispatch(
            build.collect,
            [groups.min(65535), groups.div_ceil(65535), 1],
        )?;
        commands.compute_barrier();
        commands.dispatch(build.schedule, [1, 1, 1])?;
        // 首次间接发射同步调度参数的 DRAW_INDIRECT 读取。后续阶段只读这些参数。
        commands.barrier();
        self.indirect(commands, build.sort, TILE_ARGS)?;
        let passes = groups.next_power_of_two().ilog2() as usize;
        for (i, kernel) in build.merges[..passes].iter().enumerate() {
            commands.compute_barrier();
            self.indirect(commands, *kernel, MERGE_ARGS + i as u64)?;
        }
        Ok(())
    }
    pub fn record_power(&self, commands: &mut Commands) -> Result<(), Error> {
        if let Some(build) = &self.build {
            let mut width = (build.config[0] + build.config[1]).next_power_of_two();
            for (i, kernel) in build.power.iter().enumerate() {
                commands.compute_barrier();
                self.indirect(commands, *kernel, POWER_ARGS + i as u64)?;
                width = width.div_ceil(64);
                if width == 1 {
                    break;
                }
            }
        }
        Ok(())
    }
    pub fn record_branches(&self, commands: &mut Commands) -> Result<(), Error> {
        if let Some(build) = &self.build {
            commands.compute_barrier();
            self.indirect(commands, build.radix, BRANCH_ARGS)?;
        }
        Ok(())
    }
    pub fn roots(&self, roots: &mut Vec<vulkan::native::Resource>) {
        roots.extend(
            [self.empty, self.config, self.schedule, self.nodes]
                .map(vulkan::native::Resource::from),
        );
        roots.extend(
            self.widths
                .iter()
                .copied()
                .map(vulkan::native::Resource::from),
        );
        roots.extend(self.programs.map(vulkan::native::Resource::from));
        if let Some(build) = &self.build {
            roots.extend(
                [build.collect, build.schedule, build.sort, build.radix]
                    .map(vulkan::native::Resource::from),
            );
            roots.extend(build.power.map(vulkan::native::Resource::from));
            roots.extend(
                build
                    .merges
                    .iter()
                    .copied()
                    .map(vulkan::native::Resource::from),
            );
        }
    }
}
