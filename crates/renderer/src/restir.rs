//! 原生ReSTIR工作集与ReSTCV颜色事务；场景和显式失效边界仍归Gpu。
//!
//! 两份颜色各12N字节，颜色属于像素，不跟随路径赢家或fresh输出选择。
//! 当帧反射率代理复用PT的normal暂存区，initial先写merge再读；不增加存储或跨SER活跃状态。
//! 只允许连续成功提交的CV帧读取旧颜色；开关事件仅令颜色冷启动。
//! 引擎FrameToken共同接受路径与ColorOutput生产结果，应用不另持有颜色连续性有效位。
//! 静态场景下的线性控制变量性质沿用引擎control_variate契约；当前场景重放不保证动态无偏。

use super::{Error, materials, shader};
use simple_engine::restir_pt;
use simple_engine::restir_pt::gpu::{Buffers, FusedPrograms, Programs, RayLaunch, Renderer};
use simple_engine::vulkan::native::{
    Binding, Bindings, Buffer, Commands, ComputePipeline, Context, Recorded, Resource,
};

const FUSION: restir_pt::Fusion = restir_pt::Fusion::DIRECTIONS;

pub(super) struct Restir {
    renderer: Renderer<Buffer>,
    post: ComputePipeline,
    pub previous_jitter: [f32; 2],
    pending: Option<(
        restir_pt::FrameToken,
        crate::reconstruction::CameraFrame,
        [f32; 2],
        restir_pt::ColorOutput,
    )>,
    pub previous_camera: Option<crate::reconstruction::CameraFrame>,
    #[cfg(test)]
    timestamps: Option<simple_engine::vulkan::native::TimestampQueries>,
}

fn binding_rows(
    mut scene: Vec<Binding>,
    geometry: Buffer,
    lut: Buffer,
    buffers: &Buffers<Buffer>,
) -> Vec<Binding> {
    scene.push(Binding::StorageBuffer(geometry));
    scene.extend(
        buffers
            .reservoirs()
            .iter()
            .chain(buffers.primary_hits())
            .copied()
            .map(Binding::StorageBuffer),
    );
    scene.extend(
        [*buffers.neighbor_mask(), *buffers.precomputed(), lut].map(Binding::StorageBuffer),
    );
    scene.extend(
        buffers
            .color_estimates()
            .expect("布局含独立CV颜色")
            .iter()
            .copied()
            .map(Binding::StorageBuffer),
    );
    scene
}

impl Restir {
    #[allow(
        clippy::too_many_arguments,
        reason = "资源边界显式借用描述符、callable与设备变体"
    )]
    pub fn prepare(
        context: &mut Context,
        scene: Vec<Binding>,
        post: Bindings,
        geometry: Buffer,
        lut: Buffer,
        hit: materials::HitFeatures,
        callables: &[super::RayShader<'_>],
        initial_override: Option<super::RayShader<'_>>,
        initial_hits: &[super::RayShader<'_>],
        reordered: bool,
    ) -> Result<Self, Error> {
        let buffers = Buffers::new_native(
            context,
            restir_pt::Layout::with_color_estimates([1, 1], FUSION)?,
        )?;
        let bindings = Bindings::new(context, &binding_rows(scene, geometry, lut, &buffers))?;
        let initial = initial_override.unwrap_or_else(|| {
            shader(
                if reordered {
                    include_bytes!(concat!(env!("OUT_DIR"), "/restir-initial-ser.spv"))
                } else {
                    include_bytes!(concat!(env!("OUT_DIR"), "/restir-initial.spv"))
                },
                c"restir_initial",
            )
        });
        let temporal = shader(
            if reordered {
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/restir-temporal-direction-ser.spv"
                ))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/restir-temporal-direction.spv"))
            },
            c"restir_temporal_direction",
        );
        let spatial = shader(
            if reordered {
                include_bytes!(concat!(
                    env!("OUT_DIR"),
                    "/restir-spatial-direction-ser.spv"
                ))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/restir-spatial-direction.spv"))
            },
            c"restir_spatial_direction",
        );
        let merge = shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/restir-merge.spv")),
            c"restir_merge",
        );
        let temporal_combine = shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/restir-temporal-combine.spv")),
            c"restir_temporal_combine",
        );
        let spatial_work = shader(
            include_bytes!(concat!(env!("OUT_DIR"), "/restir-spatial-work.spv")),
            c"restir_spatial_work",
        );
        let count = callables.len() / 4;
        // 命中只携带稳定身份；着色法线在真正加载表面时恢复一次。
        let trace = super::programs(
            &[],
            false,
            materials::HitFeatures {
                authored_normals: false,
                ..hit
            },
        );
        let programs = Programs {
            ray_launch: RayLaunch::Raster,
            fused: Some(FusedPrograms {
                reconnection_callables: &[],
                initial,
                temporal,
                spatial,
                merge,
                temporal_direction: temporal,
                temporal_combine,
                spatial_work,
                spatial_direction: spatial,
            }),
            primary: initial,
            initial,
            temporal_work: merge,
            temporal_replay: temporal,
            temporal_merge: temporal,
            spatial_work: merge,
            spatial_replay: spatial,
            spatial_precompute: spatial,
            spatial_merge: merge,
            resolve: merge,
            miss: trace.miss,
            closest_hit: trace.closest_hit,
            initial_hits,
            any_hit: trace.any_hit,
            initial_callables: if initial_hits.is_empty() {
                &callables[..2 * count]
            } else {
                &[]
            },
            replay_callables: &callables[2 * count..3 * count],
            evaluate_callables: &callables[3 * count..],
        };
        Ok(Self {
            renderer: Renderer::new_native(
                context,
                programs,
                bindings,
                buffers,
                restir_pt::Settings::default(),
            )?,
            post: ComputePipeline::new(
                context,
                post,
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/post.spv")),
                    c"post_main",
                ),
            )?,
            previous_jitter: [0.; 2],
            previous_camera: None,
            pending: None,
            #[cfg(test)]
            timestamps: None,
        })
    }

    pub fn extent(&self) -> [u32; 2] {
        self.renderer.buffers().layout().extent()
    }

    #[cfg(test)]
    pub fn history_buffer(&self) -> Buffer {
        self.renderer.buffers().reservoirs()[1]
    }

    #[cfg(test)]
    pub fn primary_history(&self) -> Buffer {
        self.renderer.buffers().primary_hits()[1]
    }

    #[cfg(test)]
    pub fn test_buffers(&self) -> &Buffers<Buffer> {
        self.renderer.buffers()
    }

    #[cfg(test)]
    pub fn test_stack_sizes(&self, context: &Context) -> Result<serde_json::Value, Error> {
        Ok(serde_json::json!(self.renderer.program_stack_sizes(context)?.into_iter().map(|(stage, sizes)|
            serde_json::json!({"stage":format!("{stage:?}"),"raygen":sizes.raygen,"closest_hit":sizes.closest_hit,
                "any_hit":sizes.any_hit,"callable_max":sizes.callable_max,"callable_count":sizes.callable_count})).collect::<Vec<_>>()))
    }

    #[cfg(test)]
    pub fn test_program_roots(&self, context: &Context) -> Result<String, Error> {
        Ok(format!("{:?}", self.renderer.program_roots(context)?))
    }

    #[cfg(test)]
    pub fn test_bindings(
        &self,
        context: &mut Context,
        scene: Vec<Binding>,
        geometry: Buffer,
        lut: Buffer,
        output: Option<Buffer>,
    ) -> Result<Bindings, Error> {
        let mut rows = binding_rows(scene, geometry, lut, self.renderer.buffers());
        rows.extend(output.map(Binding::StorageBuffer));
        Ok(Bindings::new(context, &rows)?)
    }

    #[cfg(test)]
    pub fn identity_probe(
        &self,
        context: &mut Context,
        scene: Vec<Binding>,
        geometry: Buffer,
        lut: Buffer,
        output: Buffer,
    ) -> Result<ComputePipeline, Error> {
        let mut rows = binding_rows(scene, geometry, lut, self.renderer.buffers());
        rows.push(Binding::StorageBuffer(output));
        let binding = Bindings::new(context, &rows)?;
        Ok(ComputePipeline::new(
            context,
            binding,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/restir_identity_probe.spv")),
                c"restir_identity_probe",
            ),
        )?)
    }

    #[cfg(test)]
    pub fn reprojection_probe(
        &self,
        context: &mut Context,
        scene: Vec<Binding>,
        geometry: Buffer,
        lut: Buffer,
        output: Buffer,
    ) -> Result<ComputePipeline, Error> {
        let mut rows = binding_rows(scene, geometry, lut, self.renderer.buffers());
        rows.push(Binding::StorageBuffer(output));
        let binding = Bindings::new(context, &rows)?;
        Ok(ComputePipeline::new(
            context,
            binding,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/restir_donor_probe.spv")),
                c"restir_donor_probe",
            ),
        )?)
    }

    pub fn resize(
        &self,
        context: &mut Context,
        scene: Vec<Binding>,
        post: Bindings,
        geometry: Buffer,
        lut: Buffer,
        extent: [u32; 2],
    ) -> Result<Self, Error> {
        let buffers = Buffers::new_native(
            context,
            restir_pt::Layout::with_color_estimates(extent, FUSION)?,
        )?;
        let bindings = Bindings::new(context, &binding_rows(scene, geometry, lut, &buffers))?;
        Ok(Self {
            renderer: self.renderer.prepare_resize(context, bindings, buffers)?,
            post: self.post.rebind(context, post)?,
            previous_jitter: [0.; 2],
            previous_camera: None,
            pending: None,
            #[cfg(test)]
            timestamps: None,
        })
    }

    pub fn rebind(
        &mut self,
        context: &mut Context,
        scene: Vec<Binding>,
        post: Bindings,
        geometry: Buffer,
        lut: Buffer,
        template: &Self,
    ) -> Result<(), Error> {
        let bindings = Bindings::new(
            context,
            &binding_rows(scene, geometry, lut, self.renderer.buffers()),
        )?;
        let prepared = self
            .renderer
            .prepare_variant(context, &template.renderer, bindings)?;
        let post = self.post.rebind(context, post)?;
        self.renderer.commit_rebind(prepared)?;
        self.post = post;
        Ok(())
    }

    pub fn frame(&self) -> Result<restir_pt::FrameParams, Error> {
        Ok(self.renderer.next_frame_params()?)
    }

    pub const fn has_history(&self) -> bool {
        self.previous_camera.is_some()
    }

    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub const fn color_history_valid(&self) -> bool {
        self.renderer.color_history_valid()
    }

    pub fn invalidate_color(&mut self, permit: super::history::ResetPermit) -> Result<(), Error> {
        use super::history::{Histories, History, Invalidation};
        self.renderer.invalidate_history(Invalidation::new(
            Histories::one(History::Color),
            permit.request().reason(),
        ))?;
        Ok(())
    }

    pub fn invalidate(&mut self, permit: super::history::ResetPermit) -> Result<(), Error> {
        use super::history::{Histories, History, Invalidation};
        self.renderer.invalidate_history(Invalidation::new(
            Histories::one(History::Paths),
            permit.request().reason(),
        ))?;
        self.previous_camera = None;
        Ok(())
    }

    pub fn record(
        &mut self,
        context: &mut Context,
        camera: crate::reconstruction::CameraFrame,
        jitter: [f32; 2],
        cv: bool,
    ) -> Result<Recorded, Error> {
        #[cfg(test)]
        let frame = if let Some(queries) = self.timestamps {
            self.renderer.record_timed(context, queries)?
        } else {
            self.renderer.record(context)?
        };
        #[cfg(not(test))]
        let frame = self.renderer.record(context)?;
        let color = if cv {
            restir_pt::ColorOutput::Produced
        } else {
            restir_pt::ColorOutput::Omitted
        };
        self.pending = Some((frame.token, camera, jitter, color));
        Ok(frame.commands)
    }

    pub fn post(&self, context: &mut Context) -> Result<Recorded, Error> {
        let mut commands = Commands::new(context)?;
        let extent = self.extent();
        commands.barrier();
        commands.dispatch(self.post, [extent[0].div_ceil(8), extent[1].div_ceil(8), 1])?;
        commands.barrier();
        Ok(commands.finish(context)?)
    }

    pub fn completed(&mut self) -> Result<(), Error> {
        if let Some((token, camera, jitter, color)) = self.pending.take() {
            self.renderer.commit_with_color(token, color)?;
            self.previous_camera = Some(camera);
            self.previous_jitter = jitter;
        }
        Ok(())
    }

    pub fn cancelled(&mut self) -> Result<(), Error> {
        if let Some((token, _, _, _)) = self.pending.take() {
            self.renderer.reject(token)?;
        }
        Ok(())
    }

    pub fn roots(&self) -> impl Iterator<Item = Resource> {
        let roots = self.renderer.roots().chain([self.post.into()]);
        #[cfg(test)]
        let roots = roots.chain(self.timestamps.map(Resource::from));
        roots
    }

    #[cfg(test)]
    pub fn test_set_timestamps(
        &mut self,
        queries: Option<simple_engine::vulkan::native::TimestampQueries>,
    ) {
        self.timestamps = queries;
    }
}
