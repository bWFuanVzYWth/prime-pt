//! 原生 ReSTIR 工作集与准备好的阶段程序；场景和完成边界仍归 Gpu。

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
    )>,
    pub previous_camera: Option<crate::reconstruction::CameraFrame>,
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
        reordered: bool,
    ) -> Result<Self, Error> {
        let buffers =
            Buffers::new_native(context, restir_pt::Layout::with_fusion([1, 1], FUSION)?)?;
        let bindings = Bindings::new(context, &binding_rows(scene, geometry, lut, &buffers))?;
        let initial = shader(
            if reordered {
                include_bytes!(concat!(env!("OUT_DIR"), "/restir-initial-ser.spv"))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/restir-initial.spv"))
            },
            c"restir_initial",
        );
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
            any_hit: trace.any_hit,
            initial_callables: &callables[..2 * count],
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
        let buffers =
            Buffers::new_native(context, restir_pt::Layout::with_fusion(extent, FUSION)?)?;
        let bindings = Bindings::new(context, &binding_rows(scene, geometry, lut, &buffers))?;
        Ok(Self {
            renderer: self.renderer.prepare_resize(context, bindings, buffers)?,
            post: self.post.rebind(context, post)?,
            previous_jitter: [0.; 2],
            previous_camera: None,
            pending: None,
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

    pub fn invalidate(&mut self, _: super::history::ResetPermit) -> Result<(), Error> {
        self.renderer.reset_history()?;
        self.previous_camera = None;
        self.previous_jitter = [0.; 2];
        Ok(())
    }

    pub fn record(
        &mut self,
        context: &mut Context,
        camera: crate::reconstruction::CameraFrame,
        jitter: [f32; 2],
    ) -> Result<Recorded, Error> {
        let frame = self.renderer.record(context)?;
        self.pending = Some((frame.token, camera, jitter));
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
        if let Some((token, camera, jitter)) = self.pending.take() {
            self.renderer.commit(token)?;
            self.previous_camera = Some(camera);
            self.previous_jitter = jitter;
        }
        Ok(())
    }

    pub fn cancelled(&mut self) -> Result<(), Error> {
        if let Some((token, _, _)) = self.pending.take() {
            self.renderer.reject(token)?;
        }
        Ok(())
    }

    pub fn roots(&self) -> impl Iterator<Item = Resource> {
        self.renderer.roots().chain([self.post.into()])
    }
}
