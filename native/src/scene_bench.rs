//! 显式运行的场景收敛基线；使用真实宿主 draw，不启动 Minecraft。
//!
//! PRIME_PT_SCENE_BENCH_OUTPUT 指定 temp/render-convergence-20261103 下的新目录。
//! 固定64个section、98,304个quad、1024×576、1spp、12顶点、普通RT、RR关闭。
//! steady每轮从绝对样本0开始；edit每次只替换一个section，消费生产增量场景入口。
//! Host::render_options包含command pool/录制、参数上传、submit、queue idle及Flight完成，
//! 是主机完成墙钟，不是GPU timestamp。source apply、merge和set_scene分别报告；
//! set_scene_complete计入set_sections的全量属性打包/上传、局部BLAS/TLAS构建与等待和回收。
//! 设备/初始场景/Slang编译/预热、reset、全部readback和文件写入不在draw计时内。
//! 工作buffer保持生产DeviceLocal分配，捕获使用计时外compute逐位复制，不改热路径布局。

use super::*;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const EXTENT: [u32; 2] = [1024, 576];
const ROUNDS: usize = 6;
const FRAMES: usize = 32;
const EDITS: usize = 8;
const EDIT_KEY: [i32; 3] = [1, 1, 1];

fn ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).expect("测量时间可用u64纳秒表示")
}

fn output_directory() -> Result<PathBuf> {
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../temp/render-convergence-20261103");
    fs::create_dir_all(&base)?;
    let base = base.canonicalize()?;
    let value = std::env::var_os("PRIME_PT_SCENE_BENCH_OUTPUT")
        .ok_or("请设置 PRIME_PT_SCENE_BENCH_OUTPUT 为新的基线目录")?;
    let requested = PathBuf::from(value);
    let target = if requested.is_absolute() {
        requested
    } else {
        base.join(requested)
    };
    let parent = target
        .parent()
        .ok_or("输出目录缺少父目录")?
        .canonicalize()?;
    if !parent.starts_with(&base) {
        return Err("输出目录必须位于 temp/render-convergence-20261103 内".into());
    }
    let target = parent.join(target.file_name().ok_or("输出目录名称无效")?);
    fs::create_dir(&target)?;
    Ok(target)
}

fn save(root: &Path, name: &str, bytes: &[u8]) -> Result<Value> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(name))?;
    file.write_all(bytes)?;
    Ok(json!({"file":name,"bytes":bytes.len(),"blake3":blake3::hash(bytes).to_hex().as_str()}))
}

fn save_json(root: &Path, name: &str, value: &Value) -> Result {
    save(root, name, &serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn word(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn source(keys: &[[i32; 3]], edited: bool) -> Vec<u8> {
    source_attributes(keys, edited, 0x00bf_d9a6, false)
}

fn source_attributes(keys: &[[i32; 3]], edited: bool, tint: u32, mirror_uv: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    word(&mut bytes, 0x3154_5250);
    word(&mut bytes, keys.len() as u32);
    for &key in keys {
        for axis in key {
            word(&mut bytes, axis as u32);
        }
        word(&mut bytes, 256);
        for y in 0..4 {
            for z in 0..8 {
                for x in 0..8 {
                    let local = (x * 2) | ((y * 4) << 4) | ((z * 2) << 8);
                    word(&mut bytes, local);
                    word(&mut bytes, 6);
                    for _ in 0..3 {
                        word(&mut bytes, 0);
                    }
                    let h = if edited && key == EDIT_KEY && local == 0 {
                        1.5
                    } else {
                        1.0
                    };
                    let faces = [
                        [[0., 0., 0.], [0., 0., 1.], [0., h, 1.], [0., h, 0.]],
                        [[1., 0., 1.], [1., 0., 0.], [1., h, 0.], [1., h, 1.]],
                        [[0., 0., 1.], [0., 0., 0.], [1., 0., 0.], [1., 0., 1.]],
                        [[0., h, 0.], [0., h, 1.], [1., h, 1.], [1., h, 0.]],
                        [[1., 0., 0.], [0., 0., 0.], [0., h, 0.], [1., h, 0.]],
                        [[0., 0., 1.], [1., 0., 1.], [1., h, 1.], [0., h, 1.]],
                    ];
                    let cutout = (x + z + y) % 8 == 0;
                    let u = if cutout {
                        [0.501, 0.999]
                    } else {
                        [0.001, 0.499]
                    };
                    let mut uv = [[u[0], 0.001], [u[1], 0.001], [u[1], 0.999], [u[0], 0.999]];
                    if mirror_uv {
                        for corner in &mut uv {
                            corner[0] = u[0] + u[1] - corner[0];
                        }
                    }
                    for face in faces {
                        word(&mut bytes, tint);
                        for (position, uv) in face.into_iter().zip(uv) {
                            for value in position.into_iter().chain(uv) {
                                word(&mut bytes, f32::to_bits(value));
                            }
                        }
                    }
                }
            }
        }
    }
    bytes
}

fn atlas() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            bytes.extend_from_slice(&[
                100 + ((x + y) % 4) * 32,
                155,
                75,
                if x >= 8 && (x + y) % 3 == 0 { 0 } else { 255 },
            ]);
        }
    }
    bytes
}

fn scene_camera() -> Camera {
    let forward = [0., -0.2f32, 0.98f32];
    let len = forward.iter().map(|v| v * v).sum::<f32>().sqrt();
    let [x, y, z] = forward.map(|v| v / len);
    let half = (60.0f32.to_radians() * 0.5).tan();
    Camera {
        origin: [32., 36., -24., 0.],
        forward: [x, y, z, 0.],
        right: [half * EXTENT[0] as f32 / EXTENT[1] as f32, 0., 0., 0.],
        up: [0., z * half, -y * half, 0.],
    }
}

// 仅用于计时外捕获，源缓冲与生产分配完全相同。
const COPY: &str = r#"
[[vk::binding(0, 0)]] StructuredBuffer<uint4> source_sum;
[[vk::binding(1, 0)]] StructuredBuffer<uint> source_count;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint4> target_sum;
[[vk::binding(3, 0)]] RWStructuredBuffer<uint> target_count;
[shader("compute")]
[numthreads(64, 1, 1)]
void capture_main(uint3 id: SV_DispatchThreadID) {
    uint size, stride;
    source_sum.GetDimensions(size, stride);
    if (id.x < size) {
        target_sum[id.x] = source_sum[id.x];
        target_count[id.x] = source_count[id.x];
    }
}
"#;

struct Capture {
    sums: Buffer,
    counts: Buffer,
    pipeline: ComputePipeline,
}

impl Capture {
    fn new(gpu: &mut Gpu, root: &Path) -> Result<Self> {
        let source = root.join("capture-source");
        fs::create_dir(&source)?;
        fs::write(source.join("capture.slang"), COPY)?;
        let compiler =
            shader_build::RuntimeCompiler::new(root.join("capture-cache"), &[], &[&source])?;
        let path = source.join("capture.slang");
        let code = compiler.compile(&shader_build::Recipe {
            source: path.to_str().ok_or("捕获shader路径不是UTF-8")?,
            entry: "capture_main",
            stage: "compute",
            output: "capture.spv",
            target: shader_build::Target::Spirv("spirv_1_4"),
            args: &["-fp-mode", "precise"],
        })?;
        save(root, "capture.spv", code.bytes())?;
        let pixels = u64::from(EXTENT[0]) * u64::from(EXTENT[1]);
        let sums = Buffer::new(&mut gpu.context, pixels * 16, true)?;
        let counts = Buffer::new(&mut gpu.context, pixels * 4, true)?;
        let view = gpu.view.as_ref().ok_or("捕获前需要已初始化视图")?;
        let bindings = Bindings::new(
            &mut gpu.context,
            &[
                Binding::StorageBuffer(view.accumulation),
                Binding::StorageBuffer(view.counts),
                Binding::StorageBuffer(sums),
                Binding::StorageBuffer(counts),
            ],
        )?;
        let pipeline = ComputePipeline::new(
            &mut gpu.context,
            bindings,
            RayShader {
                spirv: code.bytes(),
                entry: c"capture_main",
            },
        )?;
        gpu.test_roots.push(pipeline.into());
        Ok(Self {
            sums,
            counts,
            pipeline,
        })
    }

    fn save(
        &self,
        gpu: &mut Gpu,
        image: &Image,
        root: &Path,
        name: &str,
        samples: u32,
    ) -> Result<Value> {
        let mut commands = Commands::new(&gpu.context)?;
        commands.barrier();
        commands.dispatch(self.pipeline, [(EXTENT[0] * EXTENT[1]).div_ceil(64), 1, 1])?;
        commands.barrier();
        commands
            .finish(&mut gpu.context)?
            .submit_and_wait(&mut gpu.context)?;
        let mut sums = vec![0; self.sums.len() as usize];
        let mut counts = vec![0; self.counts.len() as usize];
        self.sums.read(&mut gpu.context, 0, &mut sums)?;
        self.counts.read(&mut gpu.context, 0, &mut counts)?;
        let mut hdr = Vec::with_capacity(sums.len());
        for (sum, count) in sums
            .as_chunks::<16>()
            .0
            .iter()
            .zip(counts.as_chunks::<4>().0)
        {
            let sum: [f32; 4] = std::array::from_fn(|channel| {
                f32::from_le_bytes(sum[channel * 4..channel * 4 + 4].try_into().unwrap())
            });
            let count = u32::from_le_bytes(*count);
            if count != samples || sum.iter().any(|v| !v.is_finite()) || sum[3] < 0.0 {
                return Err("积分输出含失败像素、非有限值或错误绝对样本计数".into());
            }
            for value in [
                sum[0] / count as f32,
                sum[1] / count as f32,
                sum[2] / count as f32,
                1.0,
            ] {
                hdr.extend_from_slice(&value.to_le_bytes());
            }
        }
        let display = image.download(&mut gpu.context)?;
        Ok(json!({
            "hdr":save(root, &format!("{name}.hdr.rgba32fle"), &hdr)?,
            "sum":save(root, &format!("{name}.sum.rgba32fle"), &sums)?,
            "counts":save(root, &format!("{name}.counts.u32le"), &counts)?,
            "display":save(root, &format!("{name}.display.rgba8"), &display)?,
            "samples":samples,"hdr_origin":"top-left","hdr_space":"linear BT.709",
            "display_origin":"host bottom-left"
        }))
    }
}

fn record(file: &mut File, value: &Value) -> Result {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn summarize(root: &Path) -> Result<Value> {
    let mut groups = std::collections::BTreeMap::<String, Vec<u64>>::new();
    for line in BufReader::new(File::open(root.join("samples.jsonl"))?).lines() {
        let row: Value = serde_json::from_str(&line?)?;
        for (key, value) in row.as_object().ok_or("原始样本不是对象")? {
            if key.ends_with("_ns") {
                groups
                    .entry(format!(
                        "{}/{key}",
                        row["case"].as_str().ok_or("样本缺少case")?
                    ))
                    .or_default()
                    .push(value.as_u64().ok_or("时间字段不是u64")?);
            }
        }
    }
    let mut output = serde_json::Map::new();
    for (key, mut values) in groups {
        values.sort_unstable();
        let sum: u128 = values.iter().map(|&v| u128::from(v)).sum();
        output.insert(
            key,
            json!({"samples":values.len(),"mean_ns":sum as f64 / values.len() as f64,
            "min_ns":values[0],"median_ns":values[values.len()/2],"max_ns":values[values.len()-1]}),
        );
    }
    Ok(Value::Object(output))
}

fn scene_build_metrics(gpu: &Gpu) -> Value {
    let metrics = gpu.scene_build;
    json!({
        "dirty_sections":metrics.batch.blas_count,
        "cpu_dirty_triangle_bytes":metrics.triangle_bytes,
        "cpu_dirty_triangle_capacity_bytes":metrics.triangle_capacity_bytes,
        "cpu_pending_capacity_bytes":metrics.pending_capacity_bytes,
        "cpu_input_descriptor_capacity_bytes":metrics.input_capacity_bytes,
        "cpu_attribute_capacity_bytes":metrics.attribute_capacity_bytes,
        "cpu_instance_capacity_bytes":metrics.instance_capacity_bytes,
        "blas_batch_total_ns":metrics.batch.total_ns,
        "blas_submit_wait_ns":metrics.batch.submit_wait_ns,
        "blas_input_bytes":metrics.batch.input_bytes,
        "blas_scratch_bytes":metrics.batch.scratch_bytes,
        "blas_backing_bytes":metrics.batch.backing_bytes,
        "blas_allocation_bytes":metrics.batch.allocation_bytes,
        "blas_pool_idle_bytes":metrics.batch.pool_idle_bytes,
        "cpu_phase_boundary":"dirty triangles/pending/descriptors released after batch; attributes/instances prepared afterward; excludes source meshes, cache metadata and driver memory"
    })
}

#[test]
#[ignore = "独占真实Vulkan RT设备；先设置PRIME_PT_SCENE_BENCH_OUTPUT；测量不是GPU timestamp"]
fn hardware_pt_scene_baseline() -> Result {
    let root = output_directory()?;
    let mut keys = Vec::new();
    for y in 0..4 {
        for z in 0..4 {
            for x in 0..4 {
                keys.push([x, y, z]);
            }
        }
    }
    let initial = source(&keys, false);
    let edits = [source(&[EDIT_KEY], false), source(&[EDIT_KEY], true)];
    let atlas = atlas();
    let camera = scene_camera();
    camera.validate()?;
    let environment = Environment {
        sea_level: 0.,
        ..Environment::default()
    }
    .validated()?;
    save_json(
        &root,
        "input.json",
        &json!({
            "version":2,"scene_update":"section revision cache; full attribute upload","extent":EXTENT,"sections":keys,"quads":98304,
            "edit_section":EDIT_KEY,"edit":"one cube height 1.0/1.5; replace one complete section",
            "initial":save(&root,"initial.prt",&initial)?,
            "edit_packets":[save(&root,"edit-0.prt",&edits[0])?,save(&root,"edit-1.prt",&edits[1])?],
            "atlas":save(&root,"atlas.rgba8",&atlas)?,"atlas_extent":[16,16],
            "camera":{"origin":camera.origin,"forward":camera.forward,"right":camera.right,"up":camera.up},
            "environment":{"sun_direction":environment.sun_direction,"sea_level":environment.sea_level,"exposure_ev":environment.exposure_ev},
            "integrator":{"samples":1,"max_vertices":12,"seed":0,"max_dielectric_events":128,"rr":false,"ser":false},
            "rounds":ROUNDS,"steady_frames_per_round":FRAMES,"edits_per_round":EDITS,
            "boundary":"Host::render_options: command-pool creation, draw/encode, submit, queue idle, Flight completion; readback excluded",
            "scene_update_boundary":"source apply + set_sections(full attributes/upload/dirty BLAS/TLAS wait/invalidate/collect); merge_ns=0",
            "omissions":["Minecraft/Java capture/FFI mutex","window present","RR","GPU timestamps","process peak memory"],
            "capture":"untimed compute copy; production accumulation/counts allocation unchanged"
        }),
    )?;
    let host = Host::new()?;
    let properties = unsafe { host.instance.get_physical_device_properties(host.physical) };
    let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }.to_string_lossy();
    save_json(
        &root,
        "device.json",
        &json!({"name":name,"vendor_id":properties.vendor_id,
        "device_id":properties.device_id,"driver_version":properties.driver_version,
        "api_version":properties.api_version,"validation_enabled":host.debug.is_some(),
        "profile":if cfg!(debug_assertions) { "debug" } else { "release" },
        "test_executable":std::env::current_exe()?.to_string_lossy()}),
    )?;
    let mut gpu = host.gpu()?;
    gpu.settings = pt::Settings::default();
    let mut scene = crate::scene::Scene::new()?;
    scene.apply(&initial)?;
    let start = Instant::now();
    gpu.set_atlas(16, 16, &atlas)?;
    let initial_atlas_ns = ns(start);
    let scene_started = Instant::now();
    gpu.set_sections(&scene)?;
    let initial_scene_complete_ns = ns(scene_started);
    let initial_prepare_ns = ns(start);
    let initial_scene_build = scene_build_metrics(&gpu);
    scene.dirty = false;
    let image = Image::new(
        &mut gpu.context,
        [EXTENT[0], EXTENT[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(image.into());
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 12)?;
    for _ in 0..3 {
        host.render_options(&mut gpu, &image, &camera, environment, options)?;
    }
    let capture = Capture::new(&mut gpu, &root)?;
    let mut artifacts = Vec::new();
    for (name, bytes) in [
        (
            "prefix.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/prefix.spv")).as_slice(),
        ),
        (
            "render.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/render.spv")).as_slice(),
        ),
        (
            "closesthit.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/closesthit.spv")).as_slice(),
        ),
        (
            "anyhit.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/anyhit.spv")).as_slice(),
        ),
        (
            "miss.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/miss.spv")).as_slice(),
        ),
        (
            "post.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/post.spv")).as_slice(),
        ),
        (
            "present.spv",
            include_bytes!(concat!(env!("OUT_DIR"), "/present.spv")).as_slice(),
        ),
    ] {
        artifacts.push(save(&root, name, bytes)?);
    }
    let material = gpu.generation.as_ref().ok_or("材质程序尚未发布")?;
    artifacts.push(save(
        &root,
        "callable.spv",
        material.programs.artifacts()[0].bytes(),
    )?);
    save_json(
        &root,
        "programs.json",
        &json!({"programs":artifacts,"callable_recipe":material.programs.artifacts()[0].fingerprint()}),
    )?;
    let mut raw = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("samples.jsonl"))?;
    let mut captures = Vec::new();
    for round in 0..ROUNDS {
        gpu.view.as_mut().ok_or("视图缺失")?.progress.reset();
        for frame in 0..FRAMES {
            let start = Instant::now();
            host.render_options(&mut gpu, &image, &camera, environment, options)?;
            let draw_complete_ns = ns(start);
            record(
                &mut raw,
                &json!({"case":"steady","round":round,"frame":frame,
                "first_sample":frame,"samples":1,"draw_complete_ns":draw_complete_ns}),
            )?;
        }
        captures.push(capture.save(
            &mut gpu,
            &image,
            &root,
            &format!("steady-{round}"),
            FRAMES as u32,
        )?);
    }
    // 单section更新与生产共用set_sections；编辑前的源包构造不计时。
    for round in 0..ROUNDS {
        for edit in 0..EDITS {
            let variant = (edit + 1) % 2;
            let started = Instant::now();
            scene.apply(&edits[variant])?;
            let apply_ns = ns(started);
            let merge_ns = 0u64;
            let stage = Instant::now();
            gpu.set_sections(&scene)?;
            let set_scene_complete_ns = ns(stage);
            scene.dirty = false;
            let prepare_complete_ns = ns(started);
            let stage = Instant::now();
            host.render_options(&mut gpu, &image, &camera, environment, options)?;
            let draw_complete_ns = ns(stage);
            let update_draw_complete_ns = ns(started);
            record(
                &mut raw,
                &json!({"case":"single_section_edit","round":round,"edit":edit,
                "variant":variant,"first_sample":0,"samples":1,"apply_ns":apply_ns,
                "merge_ns":merge_ns,"set_scene_complete_ns":set_scene_complete_ns,
                "prepare_complete_ns":prepare_complete_ns,"draw_complete_ns":draw_complete_ns,
                "update_draw_complete_ns":update_draw_complete_ns,"scene_build":scene_build_metrics(&gpu)}),
            )?;
        }
        captures.push(capture.save(&mut gpu, &image, &root, &format!("edit-{round}"), 1)?);
    }
    raw.flush()?;
    let errors = host.errors.load(Ordering::Relaxed);
    save_json(
        &root,
        "report.json",
        &json!({"initial_prepare_ns":initial_prepare_ns,
        "initial_atlas_ns":initial_atlas_ns,"initial_scene_complete_ns":initial_scene_complete_ns,
        "initial_scene_build":initial_scene_build,
        "raw_samples":save(&root,"samples.blake3",blake3::hash(&fs::read(root.join("samples.jsonl"))?).to_hex().as_bytes())?,
        "summary":summarize(&root)?,"captures":captures,"validation_errors":errors,
        "allocated_gpu_bytes":gpu.context.allocated_bytes(),"retained_native_cpu_bytes":gpu.context.retained_cpu_bytes(),
        "note":"logical counters exclude process peak, SDK, driver allocation and Minecraft; summaries regenerated from samples.jsonl"}),
    )?;
    if errors != 0 {
        return Err("Vulkan validation记录了错误；原始测量保留但不能作为通过基线".into());
    }
    eprintln!("场景基线完成：{}", root.display());
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；验证局部更新与anchor重定位的BLAS消费者"]
fn hardware_pt_section_update_reuses_unmodified_blas() -> Result {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = crate::scene::Scene::new()?;
    scene.apply(&source(&[[0, 0, 0], EDIT_KEY], false))?;
    gpu.set_sections(&scene)?;
    let initial: Vec<_> = gpu
        .section_cache
        .entries()
        .iter()
        .map(|entry| entry.blas)
        .collect();
    scene.apply(&source(&[EDIT_KEY], true))?;
    gpu.set_sections(&scene)?;
    let changed: Vec<_> = gpu
        .section_cache
        .entries()
        .iter()
        .map(|entry| entry.blas)
        .collect();
    assert_eq!(initial[0], changed[0]);
    assert_ne!(initial[1], changed[1]);
    scene.rebase([64., 0., 0.]);
    gpu.set_sections(&scene)?;
    assert!(
        gpu.section_cache
            .entries()
            .iter()
            .map(|entry| entry.blas)
            .eq(changed)
    );
    let image = image(&mut gpu)?;
    let mut camera = scene_camera();
    camera.origin[0] -= 64.;
    let local = host.render(&mut gpu, &image, &camera)?;
    gpu.set_scene(&scene.merge())?;
    let reference = host.render(&mut gpu, &image, &camera)?;
    assert!(
        local
            .iter()
            .zip(&reference)
            .all(|(&a, &b)| a.abs_diff(b) <= 1),
        "局部BLAS实例与原全局合批的宿主图像不等价"
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；材质属性更新保持BLAS且必须影响实际宿主图像"]
fn hardware_pt_section_attributes_reuse_blas_and_change_output() -> Result {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = crate::scene::Scene::new()?;
    scene.apply(&source(&[[0, 0, 0]], false))?;
    gpu.set_sections(&scene)?;
    let initial = gpu.section_cache.entries()[0].blas;
    let image = image(&mut gpu)?;
    let camera = Camera {
        origin: [8., 8., -12., 0.],
        forward: [0., 0., 1., 0.],
        right: [0.8, 0., 0., 0.],
        up: [0., 0.8, 0., 0.],
    };
    let before = host.render(&mut gpu, &image, &camera)?;
    scene.apply(&source_attributes(&[[0, 0, 0]], false, 0x0030_e030, true))?;
    gpu.set_sections(&scene)?;
    assert_eq!(gpu.section_cache.entries()[0].blas, initial);
    let after = host.render(&mut gpu, &image, &camera)?;
    assert_ne!(before, after, "只更新tint/UV必须传播到实际场景属性");
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
