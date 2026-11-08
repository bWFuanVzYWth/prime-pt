//! 真实 draw 入口的 CPU 录制与 GPU 全帧跨度；固定场景，不代表 Minecraft 帧率。
use super::*;
use prime_scene::{RealtimeRenderer, RenderMode, RenderRegion, RenderView};

/// 同进程完整Latin轮换；每组显式从同历史起点开始，准备和读回不计入稳态。
/// 设置PRIME_PT_DISPATCH_QUALITY_MEAN时只跑单轮，逐帧回读并以f64导出120帧均值；
/// 此独立质量运行不记录计时或阶段标记，原始末帧输出仍保留。
#[test]
#[ignore = "独占Vulkan RT/SER设备；PRIME_PT_DISPATCH_PROFILE须为新的输出目录"]
fn hardware_pt_dispatch_profile() -> Result<(), Box<dyn std::error::Error>> {
    use prime_scene::{MaterialDefinition, MaterialId};
    let directory = std::path::PathBuf::from(
        std::env::var_os("PRIME_PT_DISPATCH_PROFILE").ok_or("缺少PRIME_PT_DISPATCH_PROFILE")?,
    );
    std::fs::create_dir(&directory)?;
    let modes: Vec<u32> = std::env::var("PRIME_PT_DISPATCH_MODES")
        .unwrap_or("0,0,1,2,3".into())
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    if modes.len() < 2 || modes.iter().any(|&v| v > 8) {
        return Err("至少两个有效实验分派".into());
    }
    let source_roots: Vec<_> = std::env::var_os("PRIME_PT_DISPATCH_SOURCES")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    if !source_roots.is_empty() && source_roots.len() != modes.len() {
        return Err("shader源码目录必须与variants一一对应".into());
    }
    let select_source = |gpu: &mut Gpu, index: usize| -> Result<(), Box<dyn std::error::Error>> {
        if let Some(root) = source_roots.get(index) {
            let prime = root.join("prime");
            let engine = root.join("engine");
            gpu.compiler =
                shader_build::RuntimeCompiler::new(root.join("cache"), &[&prime, &engine], &[])?;
            gpu.shader_source = prime.join("primary.slang");
        }
        Ok(())
    };
    let quality_mean = std::env::var_os("PRIME_PT_DISPATCH_QUALITY_MEAN").is_some();
    let rounds = if quality_mean { 1 } else { modes.len() * 2 };
    let extent = [3840, 2160];
    let warmup = if quality_mean { 60 } else { 20 };
    let samples = if quality_mean { 120 } else { 30 };
    let rr = std::env::var_os("PRIME_PT_DISPATCH_RR").is_some();
    let spp: u32 = std::env::var("PRIME_PT_DISPATCH_SPP")
        .unwrap_or_else(|_| "1".into())
        .parse()?;
    let options = Options::new(rr, streamline::Quality::Dlaa, spp, 12)?;
    let cases: &[(RealtimeRenderer, bool)] =
        match std::env::var("PRIME_PT_DISPATCH_CASE").as_deref() {
            Ok("pt") => &[(RealtimeRenderer::PathTrace, false)],
            Ok("restir") => &[
                (RealtimeRenderer::RestirPt, false),
                (RealtimeRenderer::RestirPt, true),
            ],
            Err(std::env::VarError::NotPresent) => &[
                (RealtimeRenderer::PathTrace, false),
                (RealtimeRenderer::RestirPt, false),
                (RealtimeRenderer::RestirPt, true),
            ],
            _ => return Err("PRIME_PT_DISPATCH_CASE须省略或为pt/restir".into()),
        };
    let conductor = std::env::var("PRIME_PT_DISPATCH_SCENE").as_deref() == Ok("conductor");
    let stress = std::env::var("PRIME_PT_DISPATCH_SCENE").as_deref() == Ok("stress");
    let ser = std::env::var("PRIME_PT_DISPATCH_SER").as_deref() != Ok("0");
    let scale = (0.5f32).tan();
    let fixed_camera = if stress {
        Camera {
            origin: [0., 100., -180., 0.],
            forward: [0., -0.5, 0.8660254, 0.],
            right: [scale * 3840. / 2160., 0., 0., 0.],
            up: [0., scale * 0.8660254, scale * 0.5, 0.],
        }
    } else if conductor {
        Camera {
            origin: [32., 32., 48., 0.],
            forward: [0., 0., -1., 0.],
            right: [scale * 3840. / 2160., 0., 0., 0.],
            up: [0., scale, 0., 0.],
        }
    } else {
        camera()
    };
    let environment = if conductor {
        Environment {
            sea_level: 0.,
            ..Environment::default()
        }
    } else {
        Environment::default()
    };
    let host = Host::create(rr, true)?;
    let mut definitions = vec![MaterialDefinition::default(); 6];
    for material in &mut definitions {
        material.control = [0, 3, 0, 0];
    }
    definitions[1].surface[1] = 1.;
    definitions[1].base_roughness[3] = 0.15;
    definitions[2].control[0] = 1;
    definitions[2].base_roughness[3] = 0.;
    definitions[3].control[0] = 1;
    definitions[3].extinction = [1.33, 0.03, 0.01, 0.005];
    definitions[3].base_roughness[3] = 0.15;
    definitions[4].surface[2] = 0.5;
    definitions[5].emission = [2., 1., 0.5, 1.];
    let mut mesh = Mesh::default();
    for y in 0..64 {
        for x in 0..64 {
            let id = (x / 13).min(4);
            let x = x as f32 * 0.25 - 8.;
            let y = y as f32 * 0.25 - 8.;
            mesh.quads.push(Quad::with_material(
                [
                    [x, y, 8.],
                    [x, y + 0.25, 8.],
                    [x + 0.25, y + 0.25, 8.],
                    [x + 0.25, y, 8.],
                ],
                [[0.5; 2]; 4],
                [0.8, 0.7, 0.6],
                MaterialId::new(id),
            ));
        }
    }
    // 后墙接收透射路径；相机后方灯条同时覆盖太阳和局部灯的可见性路径。
    for (points, id) in [
        (
            [
                [-12., -12., 12.],
                [-12., 12., 12.],
                [12., 12., 12.],
                [12., -12., 12.],
            ],
            0,
        ),
        (
            [[-4., 3., -2.], [4., 3., -2.], [4., 4., -2.], [-4., 4., -2.]],
            5,
        ),
        (
            [
                [-10., -8., -3.],
                [10., -8., -3.],
                [10., -8., 12.],
                [-10., -8., 12.],
            ],
            0,
        ),
    ] {
        mesh.quads.push(Quad::with_material(
            points,
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(id),
        ));
    }
    if conductor {
        // 与Game的64×64×1 Gold板相同边界、相机、0.24粗糙度和线性颜色；保留各宿主真实材质访问。
        let gold = MaterialDefinition {
            base_roughness: [0.72, 0.47, 0.065, 0.24],
            surface: [1.5, 1., 0., 0.],
            ..Default::default()
        };
        definitions.fill(gold);
        mesh.quads.clear();
        let mut add = |v| {
            mesh.quads.push(Quad::with_material(
                v,
                [[0.5; 2]; 4],
                [1.; 3],
                MaterialId::new(0),
            ))
        };
        for y in 0..64 {
            for x in 0..64 {
                let (x, y) = (x as f32, y as f32);
                add([
                    [x, y, 2.],
                    [x + 1., y, 2.],
                    [x + 1., y + 1., 2.],
                    [x, y + 1., 2.],
                ]);
                add([
                    [x, y, 1.],
                    [x, y + 1., 1.],
                    [x + 1., y + 1., 1.],
                    [x + 1., y, 1.],
                ]);
            }
        }
        for v in 0..64 {
            let v = v as f32;
            add([[0., v, 1.], [0., v, 2.], [0., v + 1., 2.], [0., v + 1., 1.]]);
            add([
                [64., v, 1.],
                [64., v + 1., 1.],
                [64., v + 1., 2.],
                [64., v, 2.],
            ]);
            add([[v, 0., 1.], [v + 1., 0., 1.], [v + 1., 0., 2.], [v, 0., 2.]]);
            add([
                [v, 64., 1.],
                [v, 64., 2.],
                [v + 1., 64., 2.],
                [v + 1., 64., 1.],
            ]);
        }
    }
    if stress {
        definitions.resize(10, MaterialDefinition::default());
        definitions[6].surface = [1.5, 0.37, 0.43, 0.];
        definitions[7].control = [
            0,
            3,
            MaterialDefinition::MERS | MaterialDefinition::AUXILIARY,
            MaterialDefinition::AUTHORED_NORMAL,
        ];
        definitions[8].control = [
            0,
            1,
            MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS,
            0,
        ];
        definitions[8].uv_bounds = [0., 0., 0.5, 1.];
        definitions[8].overlay_bounds = [0.5, 0., 1., 1.];
        definitions[9].control = [3, 3, 0, 0];
        definitions[9].surface[3] = 0.2;
        definitions[9].extinction = [1.5, 0.2, 0.4, 0.6];
        mesh.quads.clear();
        for z in 0..64 {
            for x in 0..64 {
                let id = (x * 13 + z * 7) % 10;
                let h = ((x * 5 + z * 3) % 13) as f32;
                let (x, z) = (x as f32 * 4. - 128., z as f32 * 4. - 128.);
                for points in [
                    [
                        [x, h, z],
                        [x, h, z + 4.],
                        [x + 4., h, z + 4.],
                        [x + 4., h, z],
                    ],
                    [[x, 0., z], [x, h, z], [x + 4., h, z], [x + 4., 0., z]],
                    [
                        [x + 4., 0., z],
                        [x + 4., h, z],
                        [x + 4., h, z + 4.],
                        [x + 4., 0., z + 4.],
                    ],
                ] {
                    if h == 0. && points[0][1] == points[1][1] && points[0][2] == points[1][2] {
                        continue;
                    }
                    mesh.quads.push(Quad::with_material(
                        points,
                        [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
                        [0.8, 0.7, 0.6],
                        MaterialId::new(id),
                    ));
                }
            }
        }
    }
    let atlas_extent = if stress { [4, 1] } else { [1, 1] };
    let albedo: &[u8] = if stress {
        &[
            255, 255, 255, 255, 180, 210, 130, 255, 255, 255, 255, 0, 210, 230, 170, 255,
        ]
    } else {
        &[255; 4]
    };
    let mers: &[u8] = if stress {
        &[
            0, 0, 230, 0, 255, 64, 50, 0, 79, 180, 110, 95, 153, 255, 190, 179,
        ]
    } else {
        &[0, 0, 255, 0]
    };
    let auxiliary: &[u8] = if stress {
        &[
            140, 128, 30, 85, 120, 135, 80, 100, 128, 128, 0, 85, 144, 120, 120, 110,
        ]
    } else {
        &[128, 128, 255, 128]
    };
    let light_areas = vec![[u32::MAX, 0, 0, 0]; definitions.len()];
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    gpu.test_center_film = true;
    gpu.experiment_dispatch = modes[0];
    select_source(&mut gpu, 0)?;
    let started = std::time::Instant::now();
    gpu.replace_resources(ResourceUpload {
        generation: gpu.resource_generation + 1,
        extent: atlas_extent,
        albedo,
        materials: &definitions,
        mers,
        auxiliary,
        emission_alias_words: &[],
        light_areas: &light_areas,
    })?;
    let mut preparation =
        vec![serde_json::json!({"dispatch":modes[0],"host_ns":started.elapsed().as_nanos()})];
    let prepared = materials::bindings(
        gpu.resource_generation,
        &definitions,
        atlas_extent,
        albedo,
        mers,
        auxiliary,
    )?;
    let mut variants = Vec::new();
    for (index, &mode) in modes.iter().enumerate().skip(1) {
        select_source(&mut gpu, index)?;
        let started = std::time::Instant::now();
        gpu.experiment_dispatch = mode;
        let (_, generation) = gpu.prepare_material_generation(
            gpu.atlas,
            gpu.material_gpu,
            &prepared.keys,
            &definitions,
        )?;
        let previous = gpu.generation.replace(generation).unwrap();
        gpu.test_roots.extend(previous.roots());
        variants.push(Some(previous));
        preparation
            .push(serde_json::json!({"dispatch":mode,"host_ns":started.elapsed().as_nanos()}));
    }
    let last = gpu.generation.take().unwrap();
    gpu.test_roots.extend(last.roots());
    variants.push(Some(last));
    gpu.generation = variants[0].take();
    gpu.set_ser(ser)?;
    let mut active = 0;
    gpu.set_regions(RenderView {
        generation: gpu.resource_generation,
        anchor: [0.; 3],
        regions: &[RenderRegion {
            key: [0; 3],
            geometry_revision: 1,
            content_revision: 1,
            mesh: (&mesh).into(),
        }],
    })?;
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    let pair = if quality_mean {
        None
    } else {
        Some([
            vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
            vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        ])
    };
    gpu.test_roots.push(output.into());
    if let Some(pair) = pair {
        gpu.test_roots
            .extend(pair.map(vulkan::native::Resource::from));
    }
    let stage_queries = if !quality_mean && std::env::var_os("PRIME_PT_DISPATCH_STAGES").is_some() {
        let queries = vulkan::native::TimestampQueries::new(&mut gpu.context, 20)?;
        gpu.test_roots.push(queries.into());
        Some(queries)
    } else {
        None
    };
    let gpu = &mut gpu;
    if !quality_mean {
        if rr {
            let queries = vulkan::native::TimestampQueries::new(&mut gpu.context, 2)?;
            gpu.test_roots.push(queries.into());
            gpu.test_guide_timestamps = Some(queries);
        }
    }
    let mut records = Vec::new();
    for &(renderer, cv) in cases {
        for round in 0..rounds {
            for step in 0..modes.len() {
                let position = if (round / modes.len()) % 2 == 0 {
                    step
                } else {
                    modes.len() - 1 - step
                };
                let index = (position + round % modes.len()) % modes.len();
                if index != active {
                    variants[active] = gpu.generation.replace(variants[index].take().unwrap());
                    active = index;
                    gpu.renderer = None;
                    gpu.guide_pipeline = None;
                    gpu.restir_dirty = true;
                }
                if !quality_mean {
                    let queries = vulkan::native::TimestampQueries::new(
                        &mut gpu.context,
                        if modes[index] == 8 { 6 } else { 4 * spp },
                    )?;
                    gpu.test_roots.push(queries.into());
                    gpu.test_pt_timestamps = Some(queries);
                }
                gpu.set_rendering(RenderMode::Realtime, renderer)?;
                gpu.set_regions(RenderView {
                    generation: gpu.resource_generation,
                    anchor: [0.; 3],
                    regions: &[RenderRegion {
                        key: [0; 3],
                        geometry_revision: 1,
                        content_revision: 1,
                        mesh: (&mesh).into(),
                    }],
                })?;
                gpu.set_render_tuning(prime_scene::RenderTuning::new(20, 0., false, cv)?)?;
                for _ in 0..20 {
                    host.render_profiled(gpu, &output, &fixed_camera, environment, options, None)?;
                }
                if renderer == RealtimeRenderer::RestirPt {
                    gpu.restir
                        .as_mut()
                        .ok_or("缺少ReSTIR")?
                        .test_set_timestamps(stage_queries);
                }
                gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
                gpu.test_restart_sample_sequence(0)?;
                let mut timings = Vec::new();
                let mut stage_timings = Vec::new();
                let mut quality_sum = Vec::new();
                let mut quality_pixels = Vec::new();
                let quality_root_start = gpu.test_roots.len();
                let quality_readback = if quality_mean {
                    let hdr = gpu.view.as_ref().unwrap().output;
                    quality_sum.resize(hdr.len() as usize / 4, 0.0f64);
                    quality_pixels.resize(hdr.len() as usize, 0u8);
                    let readback = Buffer::new(&mut gpu.context, hdr.len(), true)?;
                    gpu.test_roots.push(readback.into());
                    Some(readback)
                } else {
                    None
                };
                let mut pt_timings = Vec::new();
                let mut primary_timings = Vec::new();
                let mut guide_timings = Vec::new();
                for frame in 0..warmup + samples {
                    if renderer == RealtimeRenderer::PathTrace {
                        gpu.transition_history(history::Event::Accumulation(
                            history::AccumulationEvent::ExplicitRequested,
                        ))?;
                    }
                    let timing = host.render_profiled(
                        gpu,
                        &output,
                        &fixed_camera,
                        environment,
                        options,
                        pair,
                    )?;
                    if frame >= warmup {
                        if let Some(readback) = quality_readback {
                            let hdr = gpu.view.as_ref().unwrap().output;
                            hdr.copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
                            readback.read(&mut gpu.context, 0, &mut quality_pixels)?;
                            for (sum, value) in quality_sum
                                .iter_mut()
                                .zip(quality_pixels.as_chunks::<4>().0)
                            {
                                let value = f32::from_le_bytes(*value);
                                if !value.is_finite() {
                                    return Err("质量均值的HDR帧包含非有限值".into());
                                }
                                *sum += f64::from(value);
                            }
                        } else {
                            timings.push(timing);
                        }
                        if renderer == RealtimeRenderer::PathTrace
                            && let Some(queries) = gpu.test_pt_timestamps
                        {
                            let ticks = queries.read(&gpu.context)?.ok_or("PT时间戳未完成")?;
                            let offset = if modes[index] == 8 { 2 } else { 0 };
                            if offset != 0 {
                                primary_timings.push(queries.elapsed_ns(
                                    &gpu.context,
                                    ticks[0],
                                    ticks[1],
                                )?);
                            }
                            pt_timings.push([
                                queries.elapsed_ns(
                                    &gpu.context,
                                    ticks[offset],
                                    ticks[offset + 1],
                                )?,
                                queries.elapsed_ns(
                                    &gpu.context,
                                    ticks[offset + 2],
                                    ticks[offset + 3],
                                )?,
                            ]);
                        }
                        if gpu.guide_pipeline.is_some()
                            && let Some(queries) = gpu.test_guide_timestamps
                        {
                            let ticks = queries.read(&gpu.context)?.ok_or("Guide时间戳未完成")?;
                            guide_timings.push(queries.elapsed_ns(
                                &gpu.context,
                                ticks[0],
                                ticks[1],
                            )?);
                        }
                        if renderer == RealtimeRenderer::RestirPt
                            && let Some(queries) = stage_queries
                        {
                            let ticks = queries
                                .read(&gpu.context)?
                                .ok_or("完成后阶段timestamp仍未ready")?;
                            let mut row = Vec::with_capacity(10);
                            for stage in 0..10 {
                                row.push(queries.elapsed_ns(
                                    &gpu.context,
                                    ticks[stage * 2],
                                    ticks[stage * 2 + 1],
                                )?);
                            }
                            stage_timings.push(row);
                        }
                    }
                }
                let pixels = if quality_mean {
                    quality_pixels
                } else {
                    let hdr = gpu.view.as_ref().unwrap().output;
                    let readback = Buffer::new(&mut gpu.context, hdr.len(), true)?;
                    hdr.copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
                    let mut pixels = vec![0; hdr.len() as usize];
                    readback.read(&mut gpu.context, 0, &mut pixels)?;
                    pixels
                };
                if !pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|w| f32::from_le_bytes(*w).is_finite())
                {
                    return Err("HDR包含非有限值".into());
                }
                let image = format!("{renderer:?}-cv{cv}-r{round}-v{index}.rgba32fle");
                std::fs::write(directory.join(&image), &pixels)?;
                let (mean_image, mean_blake3) = if quality_mean {
                    let name = format!("{renderer:?}-cv{cv}-r{round}-v{index}.mean120.rgba64fle");
                    let averaged: Vec<u8> = quality_sum
                        .iter()
                        .flat_map(|&sum| (sum / f64::from(samples)).to_le_bytes())
                        .collect();
                    let digest = blake3::hash(&averaged).to_hex().to_string();
                    std::fs::write(directory.join(&name), averaged)?;
                    (Some(name), Some(digest))
                } else {
                    (None, None)
                };
                let stacks = gpu
                    .restir
                    .as_ref()
                    .map(|r| r.test_stack_sizes(&gpu.context))
                    .transpose()?;
                let program_roots = gpu
                    .restir
                    .as_ref()
                    .map(|r| r.test_program_roots(&gpu.context))
                    .transpose()?;
                let mut parameter_bytes = vec![0; gpu.view.as_ref().unwrap().params.len() as usize];
                let material_programs = gpu.generation.as_ref().map(|generation| {
                    let roles = ["initial", "replay", "evaluate", "metadata", "connection", "emitter", "pdf", "guide_known", "initial_direct"];
                    let counts: Vec<_> = generation.restir_programs.iter()
                        .map(|set| set.artifacts().len()).collect();
                    serde_json::json!({
                        "pt_unique_modules": generation.programs.artifacts().len(),
                        "selected_emitter_capabilities":generation.emitter_capabilities,
                        "roles": roles.iter().zip(&generation.restir_programs).map(|(role, set)|
                            serde_json::json!({"role":role,"unique_modules":set.artifacts().len(),
                                "spirv_bytes":set.artifacts().iter().map(|a|a.bytes().len()).sum::<usize>()}))
                            .collect::<Vec<_>>(),
                        "guide_hit_unique_modules":counts[7],
                        "pt_native_sbt_hit_groups":1+if modes[index]==8 {generation.programs.artifacts().len()}
                            else if modes[index]>=2 {generation.programs.artifacts().len()+counts[7]}
                            else if modes[index]>0 {counts[7]} else {0},
                        "pt_guided_sbt_hit_groups":1+if modes[index]==8 {counts[7]} else if modes[index]>=2 {generation.programs.artifacts().len()+counts[7]}
                            else if modes[index]>0 {counts[7]} else {0},
                        "pt_sbt_callables":if modes[index]<2 || modes[index]==8 {generation.programs.artifacts().len()} else {0},
                        "primary_sbt_hit_groups":usize::from(modes[index]==8),
                        "primary_cache_bytes_per_pixel":if modes[index]==8 {32} else {0},
                        "guide_sbt_callables":if modes[index]==8 {counts[7]} else {0},
                        "pt_prepared_variants":1+usize::from(generation.reordered.is_some())+usize::from(generation.guide_regular.is_some())+usize::from(generation.guide_reordered.is_some()),
                        "rr_program_addition":{"pipeline_prepare_ns":generation.guide_program_cost[0],"tracked_gpu_allocation_bytes":generation.guide_program_cost[1],"retained_cpu_bytes":generation.guide_program_cost[2],"raygen_spirv_bytes":generation.guide_program_cost[3],"driver_private_memory":"not exposed"},
                        "initial_sbt_callables":if generation.dispatch==8 {0} else {counts[7]+if generation.dispatch < 2 {counts[0]+counts[8]} else {0}},
                        "shift_sbt_callables":counts[1..7].iter().sum::<usize>(),
                        "operation_table_bytes":36*generation.programs.plan().keys().len()+32,
                    })
                });
                gpu.view.as_ref().unwrap().params.read(
                    &mut gpu.context,
                    0,
                    &mut parameter_bytes,
                )?;
                let row = serde_json::json!({"renderer":format!("{renderer:?}"),"cv":cv,"round":round,"position":step,
                    "driver_stack_bytes":stacks,"immutable_programs":program_roots,"material_programs":material_programs,"frame_parameters":parameter_bytes.as_chunks::<4>().0.iter().map(|w|u32::from_le_bytes(*w)).collect::<Vec<_>>(),
                    "scene":if stress {"stress"} else if conductor {"conductor"} else {"materials"},"ser":ser,"source_root":source_roots.get(index),"variant":index,"dispatch":modes[index],"cpu_gpu_ns":timings,"gpu_stage_ns":stage_timings,"primary_ns":primary_timings,"pt_trace_post_ns":pt_timings,"guide_ns":guide_timings,"quality_only":quality_mean,"mean_frames":if quality_mean {samples} else {0},"mean_image":mean_image,"mean_blake3":mean_blake3,"image":image,"blake3":blake3::hash(&pixels).to_hex().as_str()});
                std::fs::write(
                    directory.join(format!("{renderer:?}-cv{cv}-r{round}-v{index}.json")),
                    serde_json::to_vec_pretty(&row)?,
                )?;
                records.push(row);
                // 每帧draw会回收资源；本组独占追加的readback根在所有提交和读取完成后释放。
                if quality_mean {
                    gpu.test_roots.truncate(quality_root_start);
                }
                eprintln!(
                    "dispatch profile {renderer:?} CV={cv} round={round} variant={index} mode={}",
                    modes[index]
                );
            }
        }
    }
    std::fs::write(
        directory.join("results.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"modes":modes,"rounds":rounds,
        "scene":if stress {"stress"} else if conductor {"conductor"} else {"materials"},"quads":mesh.quads.len(),"extent":extent,"warmup":warmup,"samples":samples,"spp":spp,"bounces":12,"ser":ser,"rr":rr,"jitter":[0,0],"pixel_center":true,
        "preparation":preparation,"quality_only":quality_mean,"scope":if quality_mean {"独立质量运行；逐帧readback，以f64导出120帧线性HDR均值，不可用于性能计时"} else {"CPU draw录制含回收；GPU完整draw；准备/场景更新/读回/窗口不计入稳态"},
        "stage_order":simple_engine::restir_pt::Stage::sequence(true).map(|stage|format!("{stage:?}")).collect::<Vec<_>>(),
        "stage_markers":stage_queries.is_some(),
        "records":records}))?,
    )?;
    let _ = gpu;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "独占Vulkan RT/SER设备；PRIME_PT_RENDER_BENCH_OUTPUT须指向新的JSON文件"]
fn hardware_pt_render_profile() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var_os("PRIME_PT_RENDER_BENCH_OUTPUT")
        .ok_or("缺少PRIME_PT_RENDER_BENCH_OUTPUT")?;
    let host = Host::new_ser()?;
    let bounces = std::env::var("PRIME_PT_EXPERIMENT_BOUNCES")
        .ok()
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(12);
    let mut session = host.gpu()?;
    let mut gpu = session.gpu();
    if let Some(mode) = std::env::var_os("PRIME_PT_EXPERIMENT_DISPATCH") {
        gpu.experiment_dispatch = mode.to_str().ok_or("实验分派须为0..5")?.parse()?;
        if gpu.experiment_dispatch > 5 {
            return Err("实验分派须为0..5".into());
        }
        gpu.set_atlas(1, 1, &[255; 4])?;
    }
    if std::env::var_os("PRIME_PT_EXPERIMENT_NO_SER").is_some() {
        gpu.set_ser(false)?;
    }
    let extent = if std::env::var_os("PRIME_PT_EXPERIMENT_SMALL").is_some() {
        [32, 32]
    } else {
        [3840, 2160]
    };
    let output = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(output.into());
    let queries = [
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
    ];
    gpu.test_roots
        .extend(queries.map(vulkan::native::Resource::from));
    let mut mesh = Mesh::default();
    for y in 0..64 {
        for x in 0..64 {
            let x = x as f32 * 0.5 - 16.;
            let y = y as f32 * 0.5 - 16.;
            mesh.quads.push(Quad::new(
                [
                    [x, y, 8.],
                    [x, y + 0.5, 8.],
                    [x + 0.5, y + 0.5, 8.],
                    [x + 0.5, y, 8.],
                ],
                [[0.5; 2]; 4],
                [0.8, 0.6, 0.4],
                Material::Opaque,
            ));
        }
    }
    let mut cases = Vec::new();
    for (renderer, cv) in [
        (RealtimeRenderer::PathTrace, false),
        (RealtimeRenderer::RestirPt, false),
        (RealtimeRenderer::RestirPt, true),
    ] {
        eprintln!(
            "开始case {renderer:?} CV={cv} dispatch={}",
            gpu.experiment_dispatch
        );
        gpu.set_render_tuning(prime_scene::RenderTuning::new(20, 0., false, cv)?)?;
        gpu.set_rendering(RenderMode::Realtime, renderer)?;
        gpu.set_regions(RenderView {
            generation: gpu.resource_generation,
            anchor: [0.; 3],
            regions: &[RenderRegion {
                key: [0; 3],
                geometry_revision: 1,
                content_revision: 1,
                mesh: (&mesh).into(),
            }],
        })?;
        gpu.set_regions(RenderView {
            generation: gpu.resource_generation,
            anchor: [0.; 3],
            regions: &[RenderRegion {
                key: [0; 3],
                geometry_revision: 1,
                content_revision: 1,
                mesh: (&mesh).into(),
            }],
        })?;
        let mut samples = Vec::new();
        let mut frame = 0;
        let warmup = std::time::Instant::now();
        while samples.len() < 64 {
            if renderer == RealtimeRenderer::PathTrace {
                gpu.transition_history(history::Event::Accumulation(
                    history::AccumulationEvent::ExplicitRequested,
                ))?;
            }
            let timing = host.render_profiled(
                &mut gpu,
                &output,
                &camera(),
                Environment::default(),
                Options::new(false, streamline::Quality::Dlaa, 1, bounces)?,
                Some(queries),
            )?;
            if frame >= 64 && warmup.elapsed() >= std::time::Duration::from_secs(2) {
                samples.push(timing);
            }
            frame += 1;
        }
        let pixels = output.download(&mut gpu.context)?;
        assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[..3] != [0; 3]));
        cases.push(
            serde_json::json!({"renderer":format!("{renderer:?}"), "cv":cv, "cpu_gpu_ns":samples, "frames":frame,
            "restir_requested_bytes":gpu.restir.as_ref().map(|r|r.test_buffers().requested_bytes()),
            "cv_color_bytes":gpu.restir.as_ref().map(|r|2*r.test_buffers().layout().memory().color_estimate),
            "display_blake3":blake3::hash(&pixels).to_hex().as_str()}),
        );
        eprintln!("完成case {renderer:?} CV={cv}");
    }
    let ser = gpu.invocation_reorder;
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(
        file,
        &serde_json::json!({
            "extent":extent, "quads":4096, "samples":1, "bounces":bounces, "ser":ser, "rr":false,
            "warmup_ms":2000,
            "scope":"CPU draw录制含回收；GPU timestamp包围完整draw；不含Java、场景更新与窗口呈现",
            "cases":cases
        }),
    )?;
    Ok(())
}
