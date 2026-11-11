//! 普通 PT 原始输出／DLAA 的等时间基准；固定场景和原始 HDR 指标，独立于产品设置。
//! 顶层ser控制重排；variant的split_transport选择实时2 spp的单／双输运pass。
//! stage_timings_ns依次记录首视线、guide、完整输运、post和PT总跨度。
use super::*;
use prime_scene::{MaterialDefinition, MaterialId, RenderMode, RenderRegion, RenderView};
use simple_engine::vulkan::native::Resource;
use std::{fs, io::BufWriter, path::Path};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "pt_metrics.rs"]
mod metrics;

fn integer(v: &serde_json::Value, key: &str, default: u32, max: u32) -> Result<u32> {
    let n = match v.get(key) {
        Some(n) => u32::try_from(n.as_u64().ok_or("参数须为无符号整数")?)?,
        None => default,
    };
    if n > max {
        return Err(format!("{key}超过上限{max}").into());
    }
    Ok(n)
}

fn write_floats(path: &Path, values: &[f32]) -> Result {
    let mut file = BufWriter::new(fs::File::create_new(path)?);
    file.write_all(bytemuck::cast_slice(values))?;
    file.flush()?;
    Ok(())
}

fn frame_list(
    config: &serde_json::Value,
    key: &str,
    default: &[u32],
    frames: u32,
) -> Result<Vec<u32>> {
    let Some(value) = config.get(key) else {
        return Ok(default.to_vec());
    };
    value
        .as_array()
        .ok_or("帧检查点须为数组")?
        .iter()
        .map(|v| {
            let n = u32::try_from(v.as_u64().ok_or("帧检查点须为正整数")?)?;
            if n == 0 || n > frames {
                return Err("帧检查点超出帧数".into());
            }
            Ok(n)
        })
        .collect()
}

struct Measurement {
    sums: Buffer,
    blocks: Buffer,
    config: Buffer,
    pipeline: ComputePipeline,
    extent: [u32; 2],
}

impl Measurement {
    fn new(gpu: &mut Gpu, source: Buffer, extent: [u32; 2]) -> Result<Self> {
        let sums = Buffer::new(&mut gpu.context, source.len(), false)?;
        let blocks = Buffer::new(
            &mut gpu.context,
            u64::from(extent[0].div_ceil(16)) * u64::from(extent[1].div_ceil(16)) * 16 * 32,
            true,
        )?;
        let config = Buffer::new(&mut gpu.context, 16, true)?;
        config.write(
            &mut gpu.context,
            0,
            bytemuck::cast_slice(&[extent[0], extent[1], 0, 0]),
        )?;
        let bindings = Bindings::new(
            &mut gpu.context,
            &[source, sums, blocks, config].map(Binding::StorageBuffer),
        )?;
        let source_path = gpu.shader_source.with_file_name("pt_measure.slang");
        let code = gpu.compiler.compile(&shader_build::Recipe {
            source: source_path.to_str().ok_or("测量shader路径不是UTF-8")?,
            entry: "measure",
            stage: "compute",
            output: "pt-measure.spv",
            target: shader_build::Target::Spirv("spirv_1_4"),
            args: &[],
        })?;
        let pipeline = ComputePipeline::new(
            &mut gpu.context,
            bindings,
            RayShader {
                spirv: code.bytes(),
                entry: c"measure",
            },
        )?;
        gpu.test_roots.extend([
            Resource::from(sums),
            blocks.into(),
            config.into(),
            bindings.into(),
            pipeline.into(),
        ]);
        Ok(Self {
            sums,
            blocks,
            config,
            pipeline,
            extent,
        })
    }

    fn record(&self, gpu: &mut Gpu, frame: u32) -> Result {
        self.config
            .write(&mut gpu.context, 8, bytemuck::bytes_of(&frame))?;
        let mut commands = Commands::new(&gpu.context)?;
        commands.dispatch(
            self.pipeline,
            [self.extent[0].div_ceil(16), self.extent[1].div_ceil(16), 1],
        )?;
        commands.barrier();
        commands
            .finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        Ok(())
    }
}

fn fixture(name: &str, extent: [u32; 2]) -> Result<(Vec<MaterialDefinition>, Mesh, Camera)> {
    if !matches!(name, "direct" | "occluded" | "glossy" | "cutout") {
        return Err("scene须为direct/occluded/glossy/cutout".into());
    }
    let mut materials = vec![MaterialDefinition::default(); 5];
    for material in &mut materials {
        material.control = [0, 3, 0, 0];
        material.base_roughness = [0.6, 0.6, 0.6, 0.8];
    }
    materials[1].base_roughness = [0.65, 0.13, 0.08, 0.7];
    materials[2].base_roughness = [0.1, 0.5, 0.15, 0.7];
    materials[3].base_roughness = [0.8, 0.7, 0.45, 0.27];
    materials[3].surface[1] = 1.;
    materials[4].emission = [16., 12., 8., 1.];
    let mut mesh = Mesh::default();
    let mut quad = |v, id| {
        mesh.quads.push(Quad::with_material(
            v,
            [[0.5; 2]; 4],
            [1.; 3],
            MaterialId::new(id),
        ))
    };
    quad(
        [
            [-8., -4., -2.],
            [8., -4., -2.],
            [8., -4., 18.],
            [-8., -4., 18.],
        ],
        0,
    );
    quad(
        [[-8., 6., -2.], [-8., 6., 18.], [8., 6., 18.], [8., 6., -2.]],
        0,
    );
    quad(
        [
            [-8., -4., 18.],
            [8., -4., 18.],
            [8., 6., 18.],
            [-8., 6., 18.],
        ],
        0,
    );
    quad(
        [
            [-8., -4., -2.],
            [-8., -4., 18.],
            [-8., 6., 18.],
            [-8., 6., -2.],
        ],
        1,
    );
    quad(
        [[8., -4., -2.], [8., 6., -2.], [8., 6., 18.], [8., -4., 18.]],
        2,
    );
    quad(
        [
            [-8., -4., -2.],
            [-8., 6., -2.],
            [8., 6., -2.],
            [8., -4., -2.],
        ],
        0,
    );
    if name == "direct" {
        quad(
            [
                [-2., 5.9, 7.],
                [-2., 5.9, 11.],
                [2., 5.9, 11.],
                [2., 5.9, 7.],
            ],
            4,
        );
    } else {
        quad(
            [
                [-1., 5.9, 14.],
                [-1., 5.9, 16.],
                [1., 5.9, 16.],
                [1., 5.9, 14.],
            ],
            4,
        );
        quad(
            [
                [-5., -1., 11.],
                [-5., 6., 11.],
                [5., 6., 11.],
                [5., -1., 11.],
            ],
            0,
        );
    }
    if name == "glossy" {
        quad(
            [
                [-6., -3.8, 4.],
                [-6., 3., 4.],
                [-2., 3., 8.],
                [-2., -3.8, 8.],
            ],
            3,
        );
        quad(
            [[2., -3.8, 8.], [2., 3., 8.], [6., 3., 4.], [6., -3.8, 4.]],
            3,
        );
        quad(
            [
                [-5., -3.8, 5.],
                [5., -3.8, 5.],
                [5., -3.8, 16.],
                [-5., -3.8, 16.],
            ],
            3,
        );
    }
    if name == "cutout" {
        let mut cutout = materials[0];
        cutout.control[2] = MaterialDefinition::CUTOUT;
        materials.push(cutout);
        for layer in 0..8 {
            let z = 2. + layer as f32 * 0.7;
            let x = layer as f32 * 0.11;
            mesh.quads.push(Quad::with_material(
                [
                    [-8. + x, -4., z],
                    [-8. + x, 6., z],
                    [8. + x, 6., z],
                    [8. + x, -4., z],
                ],
                [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
                [1.; 3],
                MaterialId::new(5),
            ));
        }
    }
    let scale = 0.5f32.tan();
    Ok((
        materials,
        mesh,
        Camera {
            origin: [0., 0., 0., 0.],
            forward: [0., 0., 1., 0.],
            right: [scale * extent[0] as f32 / extent[1] as f32, 0., 0., 0.],
            up: [0., scale, 0., 0.],
        },
    ))
}

fn moving_camera(base: &Camera, frame: u32, motion: bool) -> Camera {
    let mut camera = *base;
    if motion {
        // 32帧静止、32帧横移、之后静止；SDK历史连续，时间与样本数不混用。
        camera.origin[0] += 2.0 * (frame.saturating_sub(31).min(32) as f32 / 32.0);
    }
    camera
}

#[test]
#[ignore = "PT_QUALITY_CONFIG指定JSON；独占GPU，默认原生4K，RR固定DLAA"]
fn hardware_pt_equal_time_benchmark() -> Result {
    let config = fs::read(std::env::var_os("PT_QUALITY_CONFIG").ok_or("缺少PT_QUALITY_CONFIG")?)?;
    let config: serde_json::Value = serde_json::from_slice(&config)?;
    let output = Path::new(config["output"].as_str().ok_or("缺少output")?);
    fs::create_dir(output)?;
    fs::write(
        output.join("input.json"),
        serde_json::to_vec_pretty(&config)?,
    )?;
    let extent = [
        integer(&config, "width", 3840, 4096)?,
        integer(&config, "height", 2160, 4096)?,
    ];
    if extent.contains(&0) {
        return Err("尺寸不能为零".into());
    }
    let frames = integer(&config, "frames", 128, 4096)?;
    if frames == 0 {
        return Err("frames不能为零".into());
    }
    let warmup = integer(&config, "warmup", 20, 256)?;
    let timing_only = config["timing_only"].as_bool().unwrap_or(false);
    let rr = config["rr"].as_bool().unwrap_or(false);
    let motion = config["motion"].as_bool().unwrap_or(false);
    let save_means = config["save_means"].as_bool().unwrap_or(!rr);
    let save_raw_final = config["save_raw_final"].as_bool().unwrap_or(true);
    let save_block_frames = config["save_block_frames"].as_bool().unwrap_or(true);
    let reference = config
        .get("metric_reference")
        .map(|path| -> Result<_> {
            metrics::Reference::load(
                Path::new(path.as_str().ok_or("metric_reference须为路径")?),
                extent,
            )
        })
        .transpose()?;
    let rr_frames = frame_list(&config, "rr_frames", &[1, 16, 32, 48, 64, 80], frames)?;
    let raw_frames = frame_list(&config, "raw_frames", &[], frames)?;
    let mean_frames = frame_list(
        &config,
        "mean_frames",
        &[4, 16, 64, 128, 256, 512, 1024],
        frames,
    )?;
    let budgets: Vec<u64> = config
        .get("gpu_checkpoints_ms")
        .map(|v| -> Result<Vec<u64>> {
            v.as_array()
                .ok_or("gpu_checkpoints_ms须为数组")?
                .iter()
                .map(|v| {
                    let ms = v.as_u64().ok_or("GPU预算须为正整数毫秒")?;
                    if ms == 0 || ms > 600_000 {
                        return Err("GPU预算超出范围".into());
                    }
                    Ok(ms * 1_000_000)
                })
                .collect()
        })
        .transpose()?
        .unwrap_or_default();
    if budgets.windows(2).any(|v| v[0] >= v[1])
        || budgets.len() > 8
        || (!budgets.is_empty()
            && (timing_only || motion || (!rr && !save_means && reference.is_none())))
        || (reference.is_some() && budgets.is_empty())
        || (rr && !budgets.is_empty() && (save_means || reference.is_some()))
    {
        return Err(
            "等GPU时间检查点须递增且场景静止；RR保存重建帧，不使用HDR均值或参考统计".into(),
        );
    }
    let scene_name = config["scene"].as_str().unwrap_or("occluded");
    let seeds = config["seeds"].as_array().ok_or("缺少seeds")?;
    let variants = config["variants"].as_array().ok_or("缺少variants")?;
    if seeds.is_empty() || variants.is_empty() {
        return Err("seeds与variants均须非空".into());
    }
    let mut host = Host::create(rr, true)?;
    // 只收窄渲染器能力，用于隔离无 OMM 数据时的管线成本；不伪造设备支持。
    host.opacity_micromap &= config["renderer_omm"].as_bool().unwrap_or(true);
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    gpu.test_center_film = config["center_film"].as_bool().unwrap_or(!rr);
    gpu.experiment_dispatch = 8;
    gpu.set_ser(config["ser"].as_bool().unwrap_or(true))?;
    let (materials, mesh, mut base_camera) = fixture(scene_name, extent)?;
    let camera_x = config["camera_x"].as_f64().unwrap_or(0.);
    if !camera_x.is_finite() || camera_x.abs() > 4. {
        return Err("camera_x超出实验场景".into());
    }
    base_camera.origin[0] += camera_x as f32;
    let generation = gpu.resource_generation + 1;
    let textured = scene_name == "cutout";
    let texture_extent = if textured { [32, 32] } else { [1, 1] };
    let mut albedo = vec![255; (texture_extent[0] * texture_extent[1] * 4) as usize];
    if textured {
        for (i, pixel) in albedo.chunks_exact_mut(4).enumerate() {
            pixel[3] = if ((i % 32) / 2 + (i / 32) / 2) % 2 == 0 {
                255
            } else {
                0
            };
        }
    }
    let omm = config["omm"].as_bool().unwrap_or(false);
    if omm && (!textured || !host.opacity_micromap) {
        return Err("OMM测量需要cutout场景与实际KHR OMM设备".into());
    }
    let prototypes = [simple_engine::opacity::Quad {
        material: 5,
        uv: [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
    }];
    let prepare_started = std::time::Instant::now();
    gpu.replace_resources(ResourceUpload {
        generation,
        extent: texture_extent,
        albedo: &albedo,
        materials: &materials,
        mers: &[0, 0, 128, 0].repeat(albedo.len() / 4),
        auxiliary: &vec![128; albedo.len()],
        emission_alias_words: &[],
        opacity_quads: if omm { &prototypes } else { &[] },
        light_areas: &vec![[u32::MAX, 0, 0, 0]; materials.len()],
    })?;
    let resource_prepare_ns = prepare_started.elapsed().as_nanos();
    if omm && gpu.opacity.map.is_none() {
        return Err("OMM测量未实际准备硬件map".into());
    }
    let regions = [RenderRegion {
        key: [0; 3],
        geometry_revision: 1,
        content_revision: 1,
        mesh: (&mesh).into(),
    }];
    let region_started = std::time::Instant::now();
    gpu.set_regions(RenderView {
        generation,
        anchor: [0.; 3],
        regions: &regions,
    })?;
    let region_prepare_ns = region_started.elapsed().as_nanos();
    let image = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    let queries = [
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
        vulkan::native::TimestampQueries::new(&mut gpu.context, 1)?,
    ];
    gpu.test_roots.push(image.into());
    gpu.test_roots.extend(queries.map(Resource::from));
    let pt_queries = vulkan::native::TimestampQueries::new(&mut gpu.context, 6)?;
    let guide_queries = vulkan::native::TimestampQueries::new(&mut gpu.context, 2)?;
    gpu.test_roots
        .extend([pt_queries, guide_queries].map(Resource::from));
    let environment = Environment {
        sun_direction: [0.; 3],
        exposure_ev: 0.,
        sea_level: 0.,
    };
    let mut reports = Vec::new();
    let root_start = gpu.test_roots.len();
    for variant in variants {
        let name = variant["name"].as_str().ok_or("variant缺少name")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err("variant名称仅接受ASCII字母、数字、连字符、下划线".into());
        }
        let kind = variant["kind"].as_str().ok_or("variant缺少kind")?;
        let offline = kind == "offline";
        gpu.test_split_transport = variant["split_transport"].as_bool().unwrap_or(true);
        gpu.test_pt_timestamps = (!offline).then_some(pt_queries);
        gpu.test_guide_timestamps = (!offline && rr).then_some(guide_queries);

        let no_history = variant["no_history"].as_bool().unwrap_or(false);
        let path_trace = matches!(kind, "offline" | "pt");
        if !path_trace {
            return Err("未知kind".into());
        }
        if offline && rr {
            return Err("离线正确性参考不启用RR".into());
        }
        let spp = integer(variant, "spp", 1, if offline { 64 } else { 2 })?;
        if spp == 0 {
            return Err("无效spp".into());
        }
        let options = Options::new(rr, streamline::Quality::Dlaa, spp, 12)?;
        gpu.test_roots.truncate(root_start);
        gpu.set_rendering(if offline {
            RenderMode::Offline
        } else {
            RenderMode::Realtime
        })?;
        gpu.set_regions(RenderView {
            generation,
            anchor: [0.; 3],
            regions: &regions,
        })?;
        gpu.set_render_tuning(prime_scene::RenderTuning::DEFAULT)?;
        host.render_options(gpu, &image, &base_camera, environment, options)?;
        {}
        let hdr = gpu.view.as_ref().ok_or("缺少view")?.output;
        let readback = Buffer::new(&mut gpu.context, hdr.len(), true)?;
        gpu.test_roots.push(readback.into());
        let mut bytes = vec![0u8; hdr.len() as usize];
        let pixels = extent[0] as usize * extent[1] as usize;
        let measure = Measurement::new(gpu, hdr, extent)?;
        let mut means = vec![0f32; pixels * 4];
        let mut first_means: Vec<Option<Vec<f32>>> = (0..budgets.len()).map(|_| None).collect();
        for (seed_index, seed) in seeds.iter().enumerate() {
            let seed = u32::try_from(seed.as_u64().ok_or("seed无效")?)?;
            let dir = output.join(format!("{name}-{seed}"));
            fs::create_dir(&dir)?;
            gpu.settings.seed = seed;
            for _ in 0..warmup {
                if no_history {
                    gpu.transition_history(history::Event::Selected(history::Invalidation::new(
                        history::Histories::one(history::History::Paths)
                            .union(history::Histories::one(history::History::Color)),
                        GlobalReset::ExplicitRequested,
                    )))?;
                }
                if path_trace {
                    gpu.transition_history(history::Event::Accumulation(
                        history::AccumulationEvent::ExplicitRequested,
                    ))?;
                }
                host.render_options(gpu, &image, &base_camera, environment, options)?;
            }
            gpu.invalidate_history(GlobalReset::ExplicitRequested)?;
            gpu.test_restart_sample_sequence(0)?;
            let mut timings = Vec::new();
            let mut stage_timings = Vec::new();
            let mut frame_stats = Vec::new();
            let mut elapsed_gpu = 0u64;
            let mut checkpoint = 0usize;
            let mut checkpoints = Vec::new();
            let coarse_extent = [extent[0].div_ceil(16), extent[1].div_ceil(16)];
            let mut coarse_file =
                BufWriter::new(fs::File::create_new(dir.join("frames-block16.f32"))?);
            let coarse_pixels = coarse_extent[0] as usize * coarse_extent[1] as usize;
            let mut coarse_bytes = vec![0u8; coarse_pixels * 16 * 32];
            for frame in 0..frames {
                let camera = moving_camera(&base_camera, frame, motion);
                if no_history {
                    gpu.transition_history(history::Event::Selected(history::Invalidation::new(
                        history::Histories::one(history::History::Paths)
                            .union(history::Histories::one(history::History::Color)),
                        GlobalReset::ExplicitRequested,
                    )))?;
                }
                if path_trace {
                    gpu.transition_history(history::Event::Accumulation(
                        history::AccumulationEvent::ExplicitRequested,
                    ))?;
                }
                timings.push(host.render_profiled(
                    gpu,
                    &image,
                    &camera,
                    environment,
                    options,
                    Some(queries),
                )?);
                if !offline {
                    let ticks = pt_queries.read(&gpu.context)?.ok_or("PT timestamp未就绪")?;
                    let primary = pt_queries.elapsed_ns(&gpu.context, ticks[0], ticks[1])?;
                    let trace = pt_queries.elapsed_ns(&gpu.context, ticks[2], ticks[3])?;
                    let post = pt_queries.elapsed_ns(&gpu.context, ticks[4], ticks[5])?;
                    let total = pt_queries.elapsed_ns(&gpu.context, ticks[0], ticks[5])?;
                    let guide = if rr {
                        let ticks = guide_queries
                            .read(&gpu.context)?
                            .ok_or("Guide timestamp未就绪")?;
                        guide_queries.elapsed_ns(&gpu.context, ticks[0], ticks[1])?
                    } else {
                        0
                    };
                    stage_timings.push([primary, guide, trace, post, total]);
                }
                if rr
                    && (!gpu.view.as_ref().is_some_and(|view| view.rr_enabled)
                        || gpu.rr_unavailable.is_some())
                {
                    return Err("请求RR但运行时不可用，不将回退图像记为RR测量".into());
                }
                elapsed_gpu += timings.last().expect("刚插入时序")[1];
                let last = frame + 1 == frames
                    || budgets.last().is_some_and(|&budget| elapsed_gpu >= budget);
                if !timing_only {
                    measure.record(gpu, frame)?;
                    if (frame + 1) % 32 == 0 || last {
                        let count = (frame % 32 + 1) as usize;
                        let chunk = &mut coarse_bytes[..coarse_pixels * 16 * count];
                        measure.blocks.read(&mut gpu.context, 0, chunk)?;
                        if save_block_frames {
                            coarse_file.write_all(chunk)?;
                        }
                        for (index, data) in chunk.chunks_exact(coarse_pixels * 16).enumerate() {
                            let mut max = 0f32;
                            let mut energy = [0f64; 3];
                            for (i, rgba) in data.chunks_exact(16).enumerate() {
                                let values: [f32; 4] = std::array::from_fn(|c| {
                                    f32::from_le_bytes(
                                        rgba[c * 4..c * 4 + 4].try_into().expect("16B块"),
                                    )
                                });
                                if values.iter().any(|v| !v.is_finite()) {
                                    return Err("HDR非有限或输运状态失败".into());
                                }
                                max = max.max(values[3]);
                                let x = i as u32 % coarse_extent[0] * 16;
                                let y = i as u32 / coarse_extent[0] * 16;
                                let area = (extent[0] - x).min(16) * (extent[1] - y).min(16);
                                for c in 0..3 {
                                    energy[c] += f64::from(values[c]) * f64::from(area);
                                }
                            }
                            frame_stats.push(serde_json::json!({"frame": frame + 1 - count as u32 + index as u32,"max":max,"mean":energy.map(|v|v / pixels as f64)}));
                        }
                    }
                }
                let budget_ready = budgets
                    .get(checkpoint)
                    .is_some_and(|&budget| elapsed_gpu >= budget);
                let fixed_checkpoint =
                    save_means && budgets.is_empty() && mean_frames.contains(&(frame + 1));
                if !timing_only
                    && (save_means || reference.is_some())
                    && (budget_ready || fixed_checkpoint || last)
                {
                    measure
                        .sums
                        .copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
                    readback.read(&mut gpu.context, 0, &mut bytes)?;
                    for (mean, word) in means.iter_mut().zip(bytes.chunks_exact(4)) {
                        *mean = f32::from_le_bytes(word.try_into().expect("4B float"))
                            / (frame + 1) as f32;
                    }
                    while budgets
                        .get(checkpoint)
                        .is_some_and(|&budget| elapsed_gpu >= budget)
                    {
                        let budget = budgets[checkpoint] / 1_000_000;
                        let file = format!("mean-time-{budget}.f32");
                        if save_means {
                            write_floats(&dir.join(&file), &means)?;
                        }
                        let measured = reference
                            .as_ref()
                            .map(|r| {
                                r.measure(
                                    &means,
                                    first_means[checkpoint].as_deref(),
                                    &dir,
                                    &format!("time-{budget}"),
                                )
                            })
                            .transpose()?;
                        if reference.is_some() && seed_index == 0 && seeds.len() > 1 {
                            first_means[checkpoint] = Some(means.clone());
                        }
                        checkpoints.push(serde_json::json!({"requested_ms":budget,"gpu_ns":elapsed_gpu,"frames":frame+1,"file":save_means.then_some(file),"metrics":measured}));
                        checkpoint += 1;
                    }
                    if save_means && (fixed_checkpoint || (last && budgets.is_empty())) {
                        write_floats(&dir.join(format!("mean-{}.f32", frame + 1)), &means)?;
                    }
                }
                if (last && save_raw_final) || (!timing_only && raw_frames.contains(&(frame + 1))) {
                    hdr.copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
                    readback.read(&mut gpu.context, 0, &mut bytes)?;
                    if bytes.chunks_exact(4).enumerate().any(|(i, v)| {
                        let value = f32::from_le_bytes(v.try_into().expect("4B float"));
                        !value.is_finite() || (i % 4 == 3 && value < 0.)
                    }) {
                        return Err("HDR非有限".into());
                    }
                    fs::write(dir.join(format!("frame-{}.f32", frame + 1)), &bytes)?;
                }
                if rr && !timing_only && (rr_frames.contains(&(frame + 1)) || last || budget_ready)
                {
                    let resolved = gpu.view.as_ref().expect("已渲染view").rr.images[6]
                        .download(&mut gpu.context)?;
                    while budgets
                        .get(checkpoint)
                        .is_some_and(|&budget| elapsed_gpu >= budget)
                    {
                        let budget = budgets[checkpoint] / 1_000_000;
                        let file = format!("rr-time-{budget}.f16");
                        fs::write(dir.join(&file), &resolved)?;
                        checkpoints.push(serde_json::json!({"requested_ms":budget,"gpu_ns":elapsed_gpu,"frames":frame+1,"file":file}));
                        checkpoint += 1;
                    }
                    if rr_frames.contains(&(frame + 1)) || (last && budgets.is_empty()) {
                        fs::write(dir.join(format!("rr-{}.f16", frame + 1)), resolved)?;
                    }
                }
                if (frame + 1) % 32 == 0 {
                    eprintln!("PT {scene_name} {name} seed={seed} {}/{frames}", frame + 1);
                }
                if last {
                    if let Some(probes) = config["probe_pixels"].as_array() {
                        if probes.len() > 32 {
                            return Err("最多读回32个首命中探针".into());
                        }
                        let mut rows = Vec::new();
                        for probe in probes {
                            let x = probe[0].as_u64().ok_or("探针x须为整数")?;
                            let y = probe[1].as_u64().ok_or("探针y须为整数")?;
                            if x >= u64::from(extent[0]) || y >= u64::from(extent[1]) {
                                return Err("探针超出图像范围".into());
                            }
                            rows.push(serde_json::json!({"pixel":[x,y]}));
                        }
                        for (name, source) in [
                            ("ray", gpu.view.as_ref().unwrap().landing[0]),
                            ("identity", gpu.view.as_ref().unwrap().landing[5]),
                        ] {
                            source.copy_prefix_to(&mut gpu.context, &readback, source.len())?;
                            for row in &mut rows {
                                let pixel = row["pixel"][1].as_u64().unwrap()
                                    * u64::from(extent[0])
                                    + row["pixel"][0].as_u64().unwrap();
                                let mut bytes = [0; 16];
                                readback.read(&mut gpu.context, pixel * 16, &mut bytes)?;
                                let words = std::array::from_fn::<_, 4, _>(|i| {
                                    u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                                });
                                row[name] = serde_json::json!(words);
                            }
                        }
                        fs::write(
                            dir.join("primary-probes.json"),
                            serde_json::to_vec_pretty(&rows)?,
                        )?;
                    }
                    break;
                }
            }
            coarse_file.flush()?;
            if checkpoint != budgets.len() {
                return Err("达到最大帧数仍未达到GPU预算；不记作完整测量".into());
            }
            let row = serde_json::json!({"name":name,"kind":kind,"seed":seed,"frames":timings.len(),"extent":extent,"coarse_extent":coarse_extent,
                "checkpoints":checkpoints,"variant":variant,"timings_cpu_gpu_ns":timings,"stage_timings_ns":stage_timings,"stage_order":["primary","guide","transport","post","pt_total"],"frame_stats":frame_stats});
            fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&row)?)?;
            reports.push(row);
        }
    }
    let errors = host.errors.load(Ordering::Relaxed);
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"config":config,"validation_errors":errors,"records":reports,
        "preparation":{"resource_ns":resource_prepare_ns,"region_ns":region_prepare_ns,"omm":omm},
        "scope":"同分辨率真实draw GPU跨度含RR；GPU统计/读回/写盘/准备不计时。HDR用GPU float32累积，块统计每32帧回读。静态帧算术均值为离线估计；运动序列均值不作收敛指标。"}),
        )?,
    )?;
    if errors != 0 {
        return Err(format!("Vulkan验证错误{errors}，结果及原始日志已保留").into());
    }
    Ok(())
}
