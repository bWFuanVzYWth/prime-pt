//! 帧输入、工作集与命令记录；提交和取消仍由渲染所有者显式完成。

use super::*;

impl Gpu {
    pub(super) fn renderer_bindings(
        &self,
        view: &View,
        atlas: &Image,
        scene: &Scene,
        materials: MaterialGpu,
        lighting: LightingGpu,
    ) -> (Vec<Binding>, Vec<Binding>) {
        let buffer = |buffer: &Buffer| Binding::StorageBuffer(*buffer);
        let mut trace = vec![
            Binding::Acceleration(scene.top),
            buffer(&scene.quads),
            Binding::SampledImage(*atlas),
            Binding::Sampler(self.nearest),
            buffer(&view.params),
            Binding::SampledImage(self.energy),
            Binding::Sampler(self.linear),
            buffer(&view.output),
            buffer(&view.accumulation),
            buffer(&view.counts),
        ];
        trace.extend(view.landing[..5].iter().map(buffer));
        trace.push(buffer(&view.prefix));
        trace.extend([
            buffer(self.atmosphere.bindings().sky_mapping),
            Binding::SampledImage(*self.atmosphere.bindings().sky),
            Binding::SampledImage(*self.atmosphere.bindings().optical_depth),
        ]);
        trace.extend([1, 2, 3, 4, 5, 7, 8].map(|i| Binding::StorageImage(view.rr.images[i])));
        trace.extend([
            buffer(&view.landing[5]),
            buffer(&materials.table),
            Binding::SampledImage(materials.mers),
            Binding::SampledImage(materials.auxiliary),
            buffer(&lighting.meta), // 预留原活动灯序号列的绑定。
            buffer(&lighting.meta),
            buffer(&self.known_geometry.unwrap_or(self.geometry_placeholder)),
            buffer(&lighting.inverse),
            buffer(&lighting.regions),
            buffer(&materials.emission_alias),
            buffer(&scene.instance_quad_bases),
            buffer(&scene.tokens),
            Binding::LightWorld(lighting.tree),
            buffer(&self.known_geometry.unwrap_or(self.geometry_placeholder)),
            buffer(&view.signal_lanes),
            Binding::StorageImage(view.rr.images[6]),
            Binding::StorageImage(view.rr.images[0]),
            buffer(&view.optical_history[0]),
            buffer(&view.optical_history[1]),
        ]);
        let (terrain_frame, terrain_radiance, first_hits) = self.terrain_light.outputs();
        let post = vec![
            buffer(&view.params),
            buffer(&view.output),
            buffer(&view.accumulation),
            buffer(&view.counts),
            buffer(&view.prefix),
            Binding::SampledImage(*self.atmosphere.bindings().aerial_light),
            Binding::SampledImage(*self.atmosphere.bindings().aerial_transmittance),
            Binding::Sampler(self.linear),
            Binding::StorageBuffer(terrain_frame),
            Binding::SampledImage(terrain_radiance),
            Binding::StorageBuffer(first_hits),
        ];
        (trace, post)
    }

    pub(super) fn allocate_bindings(
        &mut self,
        (trace, post): (Vec<Binding>, Vec<Binding>),
    ) -> Result<(Bindings, Bindings), Error> {
        Ok((
            Bindings::new(&mut self.context, &trace)?,
            Bindings::new(&mut self.context, &post)?,
        ))
    }

    pub(super) fn create_guide_pipeline(&mut self) -> Result<RayTracingPipeline, Error> {
        let view = self.view.as_ref().expect("已创建view");
        let (trace, _) = self.renderer_bindings(
            view,
            &self.atlas,
            &self.scene,
            self.material_gpu,
            self.lighting_gpu,
        );
        let bindings = Bindings::new(&mut self.context, &trace)?;
        let generation = self.generation.as_ref().expect("完整资源世代");
        let program = if self.invocation_reorder {
            generation.guide_reordered
        } else {
            generation.guide_regular
        };
        Ok(program
            .expect("资源期已准备Guide程序")
            .rebind(&mut self.context, bindings)?)
    }

    pub(super) fn create_renderer(&mut self) -> Result<Renderer, Error> {
        let view = self.view.as_ref().expect("已创建view");
        let extent = view.extent;
        let descriptors = self.renderer_bindings(
            view,
            &self.atlas,
            &self.scene,
            self.material_gpu,
            self.lighting_gpu,
        );
        let (trace, post) = self.allocate_bindings(descriptors)?;
        let generation = self.generation.as_ref().unwrap();
        let program = generation.pt_program(self.invocation_reorder, self.diagnostic());
        Ok(program.rebind(&mut self.context, trace, post, extent)?)
    }

    #[cfg(test)]
    pub(super) fn record(
        &mut self,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        target: Binding,
    ) -> Result<Vec<Recorded>, Error> {
        self.record_frame(extent, camera, environment, target, false)
    }

    pub(super) fn record_frame(
        &mut self,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        target: Binding,
        rr_enabled: bool,
    ) -> Result<Vec<Recorded>, Error> {
        self.complete_previous()?;
        // 换光瞳或重新开启时，先回收已完成的旧 FFT 工作区，再为当前视图分配。
        if self.bloom_filter.enabled()
            && self
                .view
                .as_ref()
                .is_some_and(|v| v.presentation.bloom.is_none())
        {
            match &target {
                Binding::StorageImage(image) => self.collect_with(&[(*image).into()])?,
                _ => self.collect_with(&[])?,
            }
        }
        let samples = self.frame_samples(self.settings.samples);
        let next_absolute = self
            .absolute_sample
            .checked_add(samples)
            .ok_or(pt::Error::SampleOverflow)?;
        let diagnostic = self.diagnostic();
        if extent.into_iter().any(|n| n == 0 || n > 65536) {
            return Err(Error::Invalid("视图尺寸须属于 1..=65536"));
        }
        let mut result = Vec::with_capacity(4);
        let mut rr_reconfigured = false;
        let recording = (|| -> Result<(), Error> {
            let sky_enabled = environment.enabled();
            let solar_scale = if sky_enabled {
                self.solar_reference_scale * self.sun_gain
            } else {
                0.0
            };
            let block_emission_scale = if self.has_emission {
                self.block_emission_gain
            } else {
                1.0
            };
            if sky_enabled {
                let sky = atmosphere::View {
                    altitude_km: (0.5 + (camera.origin[1] - environment.sea_level) * 0.001)
                        .clamp(0.0, 36000.0),
                    forward: camera.forward[..3].try_into().unwrap(),
                    right: camera.right[..3].try_into().unwrap(),
                    up: camera.up[..3].try_into().unwrap(),
                    sun_direction: environment.sun_direction,
                    max_distance_km: 64.0,
                    radiance_scale: [solar_scale; 3],
                };
                self.atmosphere.prepare(&mut self.context, sky)?;
                self.refresh_shadow_scene()?;
                let active =
                    self.known_geometry.is_some() && self.initial_medium == [1., 0., 0., 0.];
                if let Some(update) = self.terrain_light.prepare(
                    sky,
                    camera.origin[..3].try_into().unwrap(),
                    std::array::from_fn(|i| self.anchor[i] + f64::from(camera.origin[i])),
                    active,
                )? {
                    self.terrain_input
                        .write(&mut self.context, 0, update.bytes())?;
                    result.push(
                        self.terrain_light
                            .record(&mut self.context, self.shadow_resolver)?,
                    );
                }
            }
            let nrd_enabled = self.requested_nrd && !rr_enabled && !self.offline && !diagnostic;
            if self.view.as_ref().is_none_or(|view| {
                view.output_extent != extent
                    || view.rr_enabled != rr_enabled
                    || view.nrd.is_some() != nrd_enabled
                    || (rr_enabled && view.quality != self.rr_quality)
            }) || (rr_enabled && self.rr_runtime.is_none())
            {
                let storage_event = match &self.view {
                    None => history::StorageEvent::Initial,
                    Some(view) if view.output_extent != extent => {
                        history::StorageEvent::ExtentChanged
                    }
                    Some(view)
                        if view.rr_enabled != rr_enabled || view.nrd.is_some() != nrd_enabled =>
                    {
                        history::StorageEvent::ReconstructionChanged
                    }
                    Some(view) if rr_enabled && view.quality != self.rr_quality => {
                        history::StorageEvent::QualityChanged
                    }
                    Some(_) => history::StorageEvent::RuntimeRecreated,
                };
                // configure 与替换前，入口契约已确认前次 evaluate 完成。
                let input = if rr_enabled {
                    if self.rr_runtime.is_none() {
                        let init = streamline::Init { ..self.rr_init };
                        self.rr_runtime = Some(unsafe { streamline::Runtime::new(init)? });
                    }
                    rr_reconfigured = true;
                    self.rr_runtime
                        .as_mut()
                        .unwrap()
                        .configure(extent, self.rr_quality)?
                } else {
                    extent
                };
                let mut next = View::new_scaled(
                    &mut self.context,
                    Some(&mut self.bloom_filter),
                    [input, extent],
                    if nrd_enabled {
                        Denoiser::Nrd
                    } else if rr_enabled {
                        Denoiser::Dlss
                    } else {
                        Denoiser::Off
                    },
                    self.rr_quality,
                    Some(
                        self.view
                            .as_ref()
                            .map_or(&self.program_view.presentation.display, |view| {
                                &view.presentation.display
                            }),
                    ),
                    Some(&self.program_view),
                )?;
                if nrd_enabled {
                    let (_, post) = self.renderer_bindings(
                        &next,
                        &self.atlas,
                        &self.scene,
                        self.material_gpu,
                        self.lighting_gpu,
                    );
                    let post = Bindings::new(&mut self.context, &post)?;
                    next.nrd = Some(NrdView::new(
                        &mut self.context,
                        &next,
                        &self.rr_init,
                        post,
                        self.atmosphere.parameters().sun_angular_radius(),
                    )?);
                }
                if !rr_enabled {
                    self.rr_runtime = None;
                }
                self.transition_history(history::Event::ViewStorage(storage_event))?;
                self.view = Some(next);
                self.renderer = None;
                self.guide_pipeline = None;
            } else if self.bloom_filter.enabled()
                && let Some(view) = &mut self.view
                && view.presentation.bloom.is_none()
            {
                view.presentation.bloom = self
                    .bloom_filter
                    .bind(&mut self.context, view.presentation.linear_hdr)?;
            }

            let realtime_pt =
                !self.offline && !diagnostic && self.generation.as_ref().unwrap().dispatch == 8;
            let composite = if !sky_enabled || self.initial_medium != [1., 0., 0., 0.] {
                Composite::None
            } else if self.terrain_light.shadowed() {
                Composite::Shadowed
            } else {
                Composite::Clear
            };
            let frame_film = rr_enabled || nrd_enabled;
            #[cfg(test)]
            let frame_film = frame_film || self.test_center_film;
            #[cfg(not(test))]
            let split = true;
            #[cfg(test)]
            let split = self.test_split_transport;
            let frame_plan = FramePlan::new(realtime_pt, split, frame_film, composite);
            if self.renderer.is_none() {
                self.renderer = Some(self.create_renderer()?);
            }
            if rr_enabled
                && self.generation.as_ref().unwrap().dispatch == 8
                && self.guide_pipeline.is_none()
            {
                self.guide_pipeline = Some(self.create_guide_pipeline()?);
            }

            let previous_camera = self.rr_previous;

            let current = CameraFrame::new(camera, self.anchor)?;
            let vectors = [camera.origin, camera.forward, camera.right, camera.up];
            let key = std::array::from_fn(|i| {
                if i < 16 {
                    vectors[i / 4][i % 4].to_bits()
                } else {
                    [
                        self.settings.seed,
                        self.settings.max_bounces,
                        self.settings.max_dielectric_events,
                        environment.sun_direction[0].to_bits(),
                        environment.sun_direction[1].to_bits(),
                        environment.sun_direction[2].to_bits(),
                        environment.sea_level.to_bits(),
                        solar_scale.to_bits(),
                        block_emission_scale.to_bits(),
                    ][i - 16]
                }
            });
            self.prepare_accumulation_history(key, rr_enabled || nrd_enabled || diagnostic)?;
            let view = self.view.as_mut().unwrap();
            let extent = view.extent;
            let batch = view.progress.begin(samples)?;
            let mut words: Vec<u32> = vectors.into_iter().flatten().map(f32::to_bits).collect();
            words[3] = environment.sea_level.to_bits();
            words.extend([extent[0], extent[1], batch.first(), batch.samples()]);
            let resolution = extent
                .into_iter()
                .max()
                .unwrap()
                .next_power_of_two()
                .ilog2();
            words.extend([
                self.settings.seed,
                self.settings.max_bounces,
                resolution,
                self.settings.max_dielectric_events,
            ]);
            words.extend(
                self.display_drt
                    .with_exposure(environment.exposure_ev.exp2())?
                    .to_array()
                    .map(f32::to_bits),
            );
            words.extend(environment.sun_direction.map(f32::to_bits));
            words.push(if sky_enabled { 1.0f32.to_bits() } else { 0 });
            words.extend(self.atmosphere.parameters().words());
            words.extend([solar_scale, solar_scale, solar_scale, 64.0].map(f32::to_bits));
            let jitter_index = self.rr_index;
            #[cfg(test)]
            let jitter_index = jitter_index - self.test_jitter_start;
            let jitter = crate::reconstruction::jitter(
                jitter_index,
                view.extent[0],
                view.output_extent[0],
                rr_enabled,
            )?;
            #[cfg(test)]
            let jitter = if self.test_center_film {
                [0.; 2]
            } else {
                jitter
            };
            let film_mode = if nrd_enabled {
                3.0
            } else if rr_enabled {
                2.0
            } else {
                0.0
            };
            #[cfg(test)]
            let film_mode = if self.test_center_film && !rr_enabled {
                1.0
            } else {
                film_mode
            };
            view.jitter = jitter;
            words.extend(
                [
                    jitter[0],
                    jitter[1],
                    if self.rr_previous.is_some() { 1.0 } else { 0.0 },
                    film_mode,
                ]
                .map(f32::to_bits),
            );
            words.extend([
                self.absolute_sample,
                block_emission_scale.to_bits(),
                (1.0 / extent[0] as f32).to_bits(),
                (1.0 / extent[1] as f32).to_bits(),
            ]);
            words.extend(
                previous_camera
                    .as_ref()
                    .unwrap_or(&current)
                    .camera_words(self.anchor)?
                    .map(f32::to_bits),
            );
            words.extend([
                view.output_extent[0],
                view.output_extent[1],
                u32::from(diagnostic)
                    | (u32::from(
                        self.generation
                            .as_ref()
                            .is_some_and(|g| g.hit.authored_normals),
                    ) << 2)
                    | (u32::from(realtime_pt) << 5),
                self.rr_index & 1,
            ]);
            words.extend(self.anchor.map(|v| ((v / 64.) as i32) as u32));
            words.push(0);
            words.extend(self.initial_medium.map(f32::to_bits));
            words.extend([
                (extent[0] as f32 / view.output_extent[0] as f32).to_bits(),
                (extent[1] as f32 / view.output_extent[1] as f32).to_bits(),
                self.generation
                    .as_ref()
                    .unwrap()
                    .programs
                    .plan()
                    .keys()
                    .len() as u32,
                simple_engine::light_sampling::cone_chord_squared(
                    self.atmosphere.parameters().sun_angular_radius(),
                )
                .to_bits(),
            ]);
            words.extend(current.camera_words(self.anchor)?.map(f32::to_bits));

            view.params
                .write(&mut self.context, 0, bytemuck::cast_slice(&words))?;
            if sky_enabled && let Some(recorded) = self.atmosphere.record(&mut self.context)? {
                result.push(recorded);
            }
            if batch.first() == 0 {
                let mut clear = Commands::new(&self.context)?;
                clear.clear_buffer(view.accumulation)?;
                clear.clear_buffer(view.counts)?;
                clear.barrier();
                result.push(clear.finish(&mut self.context)?);
            }
            if rr_enabled
                && !realtime_pt
                && let Some(pipeline) = self.guide_pipeline
            {
                let mut commands = Commands::new(&self.context)?;
                #[cfg(test)]
                if let Some(queries) = self.test_guide_timestamps {
                    commands.reset_timestamps(queries)?;
                    commands.barrier();
                    commands.write_timestamp(queries, 0)?;
                }
                commands.trace(pipeline, view.extent)?;
                commands.barrier();
                #[cfg(test)]
                if let Some(queries) = self.test_guide_timestamps {
                    commands.write_timestamp(queries, 1)?;
                }
                result.push(commands.finish(&mut self.context)?);
            }
            {
                let renderer = self.renderer.as_ref().unwrap();
                #[cfg(test)]
                let (queries, guide_queries) =
                    (self.test_pt_timestamps, self.test_guide_timestamps);
                #[cfg(not(test))]
                let (queries, guide_queries) = (None, None);
                let recorded = if let Some(schedule) = frame_plan.realtime {
                    renderer.record_realtime(
                        &mut self.context,
                        self.guide_pipeline.filter(|_| rr_enabled),
                        RealtimeFrame {
                            schedule,
                            primary: frame_plan.primary,
                            post: frame_plan.post,
                            queries,
                            guide_queries,
                        },
                    )?
                } else if let Some(queries) = queries {
                    renderer.record_with_timestamps(
                        &mut self.context,
                        batch.samples(),
                        queries,
                        frame_plan.post,
                    )?
                } else {
                    renderer.record(&mut self.context, batch.samples(), frame_plan.post)?
                };
                result.push(recorded);
            }
            if let Some(nrd) = &view.nrd {
                result.push(nrd.prepare(
                    &mut self.context,
                    view.extent,
                    current.camera_words(self.anchor)?,
                    view.jitter,
                )?);
                batch.commit();
                return Ok(());
            }
            if rr_enabled {
                let mut pack = Commands::new(&self.context)?;
                pack.barrier();
                pack.dispatch(view.pack, [extent[0].div_ceil(8), extent[1].div_ceil(8), 1])?;
                pack.barrier();
                result.push(pack.finish(&mut self.context)?);
                batch.commit();
                return Ok(());
            }
            let frame = if self.offline {
                simple_engine::display::exposure::Frame::new(
                    extent,
                    true,
                    0.,
                    false,
                    true,
                    self.display_timeline.compensation(),
                )?
            } else {
                self.display_timeline
                    .prepare(extent, true, self.display_origin.elapsed())?
            };
            let meter = self.display_timeline.enabled() && (!self.offline || !self.offline_metered);
            if let Some(exposure) =
                view.presentation
                    .display
                    .record(&mut self.context, frame, false, meter)?
            {
                result.push(exposure);
            }
            if self.offline {
                self.offline_metered = true;
            }
            result.push(view.presentation.record(
                &mut self.context,
                target,
                HdrSource::Raw,
                self.display_timeline.enabled(),
            )?);
            batch.commit();
            Ok(())
        })();
        if let Err(error) = recording {
            self.display_timeline.cancel();
            // SDK 配置可能已改变；宿主已确认前次完成，下一帧必须重建并重新配置。
            if rr_reconfigured && self.rr_runtime.take().is_some() {
                self.transition_history(history::Event::RrStorage(
                    history::StorageEvent::RuntimeRecreated,
                ))?;
            }
            self.cancel_recordings(result)?;
            return Err(error);
        }
        self.pending_absolute = Some((next_absolute, result.len()));
        Ok(result)
    }

    pub(super) fn cancel_recordings(&mut self, recordings: Vec<Recorded>) -> Result<(), Error> {
        self.terrain_light.cancel();
        self.pending_absolute = None;

        if self.offline {
            self.offline_metered = false;
        }
        for recorded in recordings {
            self.context.cancel(recorded)?;
        }
        self.collect_resources()
    }

    /// 宿主独占在 render pass 外录制的 command，且保活命令至 Flight 完成。
    pub(super) unsafe fn encode_recordings(
        &mut self,
        recordings: impl IntoIterator<Item = Recorded>,
        command: u64,
    ) -> Result<(), Error> {
        let mut pending = recordings.into_iter();
        while let Some(recorded) = pending.next() {
            // SAFETY: 调用者维护宿主命令的独占录制与完成契约。
            match unsafe { recorded.try_encode(&mut self.context, command) } {
                Ok(flight) => {
                    self.flights.push(flight);
                    self.recording_encoded();
                }
                Err((error, recorded)) => {
                    self.pending_absolute = None;

                    self.context.cancel(recorded)?;
                    for recorded in pending {
                        self.context.cancel(recorded)?;
                    }
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    // 只用于 RR 前的自有记录；RR 和呈现继续按顺序编码进宿主 command。
    pub(super) fn submit_diagnostic_recordings(
        &mut self,
        recordings: Vec<Recorded>,
    ) -> Result<(), Error> {
        let mut pending = recordings.into_iter().enumerate();
        let sample = self.absolute_sample;
        while let Some((record, recorded)) = pending.next() {
            let result = recorded.submit_diagnostic_and_wait(
                &mut self.context,
                |op, kind, extent, complete| {
                    let state = if complete { "complete" } else { "begin" };
                    eprintln!(
                        "[prime_pt] gpu_checkpoint sample={sample} record={record} op={op} kind={kind} extent={extent:?} state={state}"
                    );
                },
            );
            if let Err(error) = result {
                eprintln!(
                    "[prime_pt] gpu_checkpoint sample={sample} record={record} failed: {error}"
                );
                self.cancel_recordings(pending.map(|(_, record)| record).collect())?;
                return Err(error.into());
            }
            self.recording_encoded();
        }
        Ok(())
    }

    /// 句柄、GENERAL 布局和前次完成保证由 `prime_draw` 的外部契约维护。
    #[allow(
        clippy::too_many_arguments,
        reason = "宿主 command、image/view 与帧输入的直接边界"
    )]
    pub fn draw(
        &mut self,
        command: u64,
        image: u64,
        view: u64,
        extent: [u32; 2],
        camera: &Camera,
        environment: Environment,
        options: Options,
    ) -> Result<(), Error> {
        if self.generation.is_none() {
            return Err(Error::Invalid("draw前需要完整资源准备"));
        }
        if [command, image, view].contains(&0) {
            return Err(Error::Invalid("宿主 draw 句柄为空"));
        }
        self.context
            .set_diagnostic_scope(u64::from(self.absolute_sample));
        // SAFETY: prime_draw 借出本设备的 GENERAL storage view，并保活至本次命令完成。
        let target =
            unsafe { ExternalBinding::new(&self.context, view, ExternalKind::StorageImage)? };
        let diagnostic = self.diagnostic();
        let rr = options.denoiser == Denoiser::Dlss && !self.offline && !diagnostic;
        let quality_changed = self.rr_quality != options.quality;
        let output_changed = self
            .view
            .as_ref()
            .is_some_and(|view| view.output_extent != extent);
        self.settings.samples = options.samples;
        self.settings.max_bounces = options.bounces;
        self.rr_quality = options.quality;
        if !rr || quality_changed || output_changed {
            self.rr_unavailable = None;
        }
        let requested = rr && self.rr_unavailable.is_none();
        self.requested_nrd =
            options.denoiser == Denoiser::Nrd || (rr && !requested && options.allow_nrd);
        // record_frame 不编码宿主 command；SDK 初始化失败仍可安全重新准备原始 PT。
        let mut recordings = match self.record_frame(
            extent,
            camera,
            environment,
            Binding::External(target),
            requested,
        ) {
            Ok(recordings) => recordings,
            Err(Error::Streamline(error)) if requested => {
                eprintln!(
                    "RR 不可用，回退到{}：{error}",
                    if options.allow_nrd {
                        "NRD"
                    } else {
                        "原始 PT"
                    }
                );
                self.rr_unavailable = Some(error);
                self.rr_runtime = None;
                self.requested_nrd = options.allow_nrd;
                self.record_frame(
                    extent,
                    camera,
                    environment,
                    Binding::External(target),
                    false,
                )?
            }
            Err(error) => return Err(error),
        };
        let mut rr_touched = false;
        let result = (|| -> Result<(), Error> {
            let rr = self.view.as_ref().unwrap().rr_enabled;
            let nrd = self.view.as_ref().unwrap().nrd.is_some();
            let next_index = if rr || nrd {
                self.rr_index
                    .checked_add(1)
                    .ok_or(pt::Error::SampleOverflow)?
            } else {
                self.rr_index
            };
            if self.diagnostic_sync {
                eprintln!(
                    "[prime_pt] gpu_frame sample={} rr_index={} renderer=pt rr={rr} nrd={nrd} quality={:?} ser={} output={extent:?} input={:?} regions={} anchor={:?} camera={:?} records={}",
                    self.absolute_sample,
                    self.rr_index,
                    self.rr_quality,
                    self.invocation_reorder,
                    self.view.as_ref().unwrap().extent,
                    self.region_cache.entries().len(),
                    self.anchor,
                    camera.origin,
                    recordings.len(),
                );
            }
            if self.diagnostic_sync && rr {
                self.submit_diagnostic_recordings(std::mem::take(&mut recordings))?;
            } else {
                // SAFETY: 宿主独占正在 render pass 外录制的 command；会话持有 Flight 至完成。
                unsafe {
                    self.encode_recordings(std::mem::take(&mut recordings), command)?;
                }
            }
            if rr {
                let current = CameraFrame::new(camera, self.anchor)?;
                let reset = self.rr_previous.is_none();
                let view = self.view.as_ref().unwrap();
                let frame = current.frame(
                    command,
                    self.rr_index,
                    reset,
                    view.jitter,
                    self.rr_previous.as_ref().unwrap_or(&current),
                    view.rr.resources(&self.context)?,
                )?;
                rr_touched = true;
                // SAFETY: 宿主独占本设备有效的录制中 command；无检查点模式时不录入命令。
                unsafe { self.context.diagnostic_checkpoint(command, "rr/begin")? };
                if self.diagnostic_sync {
                    eprintln!(
                        "[prime_pt] gpu_checkpoint sample={} state=rr_record_begin",
                        self.absolute_sample
                    );
                }
                let evaluated = match unsafe { self.rr_runtime.as_mut().unwrap().evaluate(&frame) }
                {
                    Ok(()) => true,
                    Err(error) if error.unsafe_recording() => return Err(error.source.into()),
                    Err(error) => {
                        eprintln!("RR 本帧回退：{error}");
                        self.rr_unavailable = Some(error.source);
                        false
                    }
                };
                // SAFETY: 与 RR 前相同的宿主录制边界。
                unsafe { self.context.diagnostic_checkpoint(command, "rr/end")? };
                let measurement_extent = if evaluated {
                    view.output_extent
                } else {
                    view.extent
                };
                let frame = self.display_timeline.prepare(
                    measurement_extent,
                    true,
                    self.display_origin.elapsed(),
                )?;
                let exposure = view.presentation.display.record(
                    &mut self.context,
                    frame,
                    evaluated,
                    self.display_timeline.enabled(),
                )?;
                recordings.extend(exposure);
                let source = if evaluated {
                    HdrSource::Reconstructed
                } else {
                    HdrSource::ScaledRaw
                };
                recordings.push(view.presentation.record(
                    &mut self.context,
                    Binding::External(target),
                    source,
                    self.display_timeline.enabled(),
                )?);
                // SAFETY: 同本帧前序命令，宿主持有并独占 command 至完成。
                unsafe {
                    self.encode_recordings(std::mem::take(&mut recordings), command)?;
                }
                if self.diagnostic_sync {
                    eprintln!(
                        "[prime_pt] gpu_checkpoint sample={} state=rr_present_encoded evaluated={evaluated}",
                        self.absolute_sample
                    );
                }
                if evaluated {
                    self.rr_previous = Some(current);
                } else {
                    self.transition_history(history::Event::RrEvaluationFailed)?;
                }
                self.rr_index = next_index;
            }
            if nrd {
                let current = CameraFrame::new(camera, self.anchor)?;
                let [projection, world_view] = current.nrd_matrices(self.anchor)?;
                let view = self.view.as_mut().unwrap();
                let denoiser = view.nrd.as_mut().unwrap();
                let now = std::time::Instant::now();
                let gains = [self.sun_gain, self.block_emission_gain];
                let light = (environment.sun_direction, environment.enabled(), gains);
                let light_cut = denoiser
                    .light
                    .is_none_or(|(direction, sky, previous_gains)| {
                        sky != light.1
                            || (light.1
                                && direction
                                    .into_iter()
                                    .zip(light.0)
                                    .map(|(a, b)| a * b)
                                    .sum::<f32>()
                                    < 0.98)
                            || gains
                                .into_iter()
                                .zip(previous_gains)
                                .any(|(a, b)| a != b && (a >= 2. * b || b >= 2. * a))
                    });
                let reset = light_cut
                    || match self.rr_previous {
                        Some(previous) => current.camera_cut(previous)?,
                        None => true,
                    };
                let [previous_projection, previous_view] = if reset {
                    [projection, world_view]
                } else {
                    self.rr_previous
                        .expect("连续帧已提交上一相机")
                        .nrd_matrices(self.anchor)?
                };
                let frame = vulkan::nrd::Frame {
                    projection,
                    previous_projection,
                    view: world_view,
                    previous_view,
                    jitter: view.jitter,
                    previous_jitter: if reset {
                        view.jitter
                    } else {
                        denoiser.previous_jitter
                    },
                    sun: environment.sun_direction,
                    milliseconds: if reset {
                        16.667
                    } else {
                        (now - denoiser.last_frame)
                            .as_secs_f32()
                            .mul_add(1000., 0.)
                            .clamp(1., 250.)
                    },
                    index: self.rr_index,
                    reset: reset.into(),
                };
                rr_touched = true;
                // SAFETY: draw 在同队列完成前持有视图，此处在 PT 输入屏障之后录制。
                unsafe { denoiser.evaluate(command, &frame)? };
                recordings.push(denoiser.compose(&mut self.context, view.extent)?);
                let exposure = self.display_timeline.prepare(
                    view.extent,
                    true,
                    self.display_origin.elapsed(),
                )?;
                recordings.extend(view.presentation.display.record(
                    &mut self.context,
                    exposure,
                    false,
                    self.display_timeline.enabled(),
                )?);
                recordings.push(view.presentation.record(
                    &mut self.context,
                    Binding::External(target),
                    HdrSource::Raw,
                    self.display_timeline.enabled(),
                )?);
                // SAFETY: 与本帧前序命令共用同一提交边界。
                unsafe { self.encode_recordings(std::mem::take(&mut recordings), command)? };
                let denoiser = self.view.as_mut().unwrap().nrd.as_mut().unwrap();
                denoiser.last_frame = now;
                denoiser.previous_jitter = frame.jitter;
                denoiser.light = Some(light);
                self.rr_previous = Some(current);
                self.rr_index = next_index;
            }
            self.collect_resources()?;
            self.atmosphere_pending = true;
            Ok(())
        })();
        if result.is_err() {
            self.display_timeline.cancel();
            self.cancel_recordings(recordings)?;
            self.transition_history(history::Event::Accumulation(
                history::AccumulationEvent::RecordingCancelled,
            ))?;
            if rr_touched {
                self.transition_history(history::Event::RrEvaluationFailed)?;
            }
        }
        result
    }
}
