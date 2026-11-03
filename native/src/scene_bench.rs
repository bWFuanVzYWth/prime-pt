//! 显式运行的场景收敛基线；使用真实宿主 draw，不启动 Minecraft。
//!
//! PRIME_PT_SCENE_BENCH_OUTPUT 指定 temp/prime-lod0-20261103 下的新目录。
//! 真实LOD0窗口含1728个section，其中固定64个非空、98,304个quad；1024×576、1spp、12顶点、普通RT、RR关闭。
//! steady每轮从绝对样本0开始；edit每次只替换一个section，消费生产增量场景入口。
//! Host::render_options包含command pool/录制、参数上传、submit、queue idle及Flight完成，
//! 是主机完成墙钟，不是GPU timestamp。plan、PRT2应答、apply、prepare/GPU/commit另行报告；
//! manager_complete包含全部空源应答和渐进发布，不再等同旧直接apply 64 section的口径。
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
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../temp/prime-lod0-20261103");
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
        return Err("输出目录必须位于 temp/prime-lod0-20261103 内".into());
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

fn section_body(key: [i32; 3], edited: bool, tint: u32, mirror_uv: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
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
    bytes
}

type Sources = std::collections::BTreeMap<[i32; 3], Vec<u8>>;

fn sources(keys: &[[i32; 3]], edited: bool, tint: u32, mirror_uv: bool) -> Sources {
    keys.iter()
        .map(|&key| (key, section_body(key, edited, tint, mirror_uv)))
        .collect()
}

fn packet(requests: &[crate::scene::CaptureRequest], sources: &Sources) -> Vec<u8> {
    let mut bytes = Vec::new();
    word(&mut bytes, 0x3254_5250);
    word(&mut bytes, requests.len() as u32);
    for request in requests {
        let key = request.section;
        bytes.extend_from_slice(&request.id.to_le_bytes());
        if let Some(body) = sources.get(&key) {
            bytes.extend_from_slice(body);
        } else {
            for axis in key {
                word(&mut bytes, axis as u32);
            }
            word(&mut bytes, 0);
        }
    }
    bytes
}

fn world_camera(camera: Camera) -> crate::scene::WorldCamera {
    crate::scene::WorldCamera {
        origin: std::array::from_fn(|i| f64::from(camera.origin[i])),
        forward: camera.forward,
        right: camera.right,
        up: camera.up,
    }
}

#[derive(Default)]
struct Drive {
    plan_ns: u64,
    packet_ns: u64,
    apply_ns: u64,
    prepare_publish_ns: u64,
    gpu_scene_ns: u64,
    total_ns: u64,
    steps: usize,
    requests: usize,
    publications: usize,
    blas_count: usize,
    blas_total_ns: u64,
    blas_submit_wait_ns: u64,
    blas_input_bytes: u64,
    max_blas_scratch_bytes: u64,
    max_dirty_triangle_capacity_bytes: u64,
    packets: Vec<Vec<u8>>,
}

impl Drive {
    fn report(&self) -> Value {
        json!({"plan_ns":self.plan_ns,"packet_response_ns":self.packet_ns,
            "apply_ns":self.apply_ns,"prepare_publish_ns":self.prepare_publish_ns,
            "gpu_scene_ns":self.gpu_scene_ns,"manager_complete_ns":self.total_ns,
            "steps":self.steps,"capture_requests":self.requests,"publications":self.publications,
            "blas_count":self.blas_count,"blas_total_ns":self.blas_total_ns,
            "blas_submit_wait_ns":self.blas_submit_wait_ns,"blas_input_bytes":self.blas_input_bytes,
            "max_blas_scratch_bytes":self.max_blas_scratch_bytes,
            "max_dirty_triangle_capacity_bytes":self.max_dirty_triangle_capacity_bytes})
    }
}

// 与生产相同的plan→PRT2应答→prepare→GPU准备→commit；空section同样经过manager。
fn settle(
    scene: &mut crate::scene::Scene,
    gpu: &mut Gpu,
    camera: &crate::scene::WorldCamera,
    sources: &Sources,
) -> Result<Drive> {
    let started = Instant::now();
    let mut metrics = Drive::default();
    scene.rebase(camera.anchor()?);
    for _ in 0..20_000 {
        metrics.steps += 1;
        let stage = Instant::now();
        let requests = scene.plan(camera, 64)?.to_vec();
        metrics.plan_ns += ns(stage);
        metrics.requests += requests.len();
        if !requests.is_empty() {
            let stage = Instant::now();
            let bytes = packet(&requests, sources);
            metrics.packet_ns += ns(stage);
            let stage = Instant::now();
            scene.apply(&bytes)?;
            metrics.apply_ns += ns(stage);
            metrics.packets.push(bytes);
        }
        let stage = Instant::now();
        if scene.prepare()? {
            let gpu_started = Instant::now();
            if let Err(error) = gpu.set_sections(scene) {
                scene.reject()?;
                return Err(error.into());
            }
            metrics.gpu_scene_ns += ns(gpu_started);
            metrics.publications += 1;
            let build = gpu.scene_build;
            metrics.blas_count += build.batch.blas_count;
            metrics.blas_total_ns += build.batch.total_ns;
            metrics.blas_submit_wait_ns += build.batch.submit_wait_ns;
            metrics.blas_input_bytes += build.batch.input_bytes;
            metrics.max_blas_scratch_bytes = metrics
                .max_blas_scratch_bytes
                .max(build.batch.scratch_bytes);
            metrics.max_dirty_triangle_capacity_bytes = metrics
                .max_dirty_triangle_capacity_bytes
                .max(build.triangle_capacity_bytes);
            scene.commit()?;
        }
        metrics.prepare_publish_ns += ns(stage);
        if !scene.pending() {
            metrics.total_ns = ns(started);
            return Ok(metrics);
        }
    }
    Err("真实LOD0窗口未在有界迭代内收敛".into())
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
    let initial = sources(&keys, false, 0x00bf_d9a6, false);
    let edits = [
        sources(&[EDIT_KEY], false, 0x00bf_d9a6, false),
        sources(&[EDIT_KEY], true, 0x00bf_d9a6, false),
    ];
    let atlas = atlas();
    let camera = scene_camera();
    camera.validate()?;
    let world = world_camera(camera);
    let mut templates = Vec::new();
    for (&key, bytes) in &initial {
        templates.push(json!({"section":key,"body":save(&root, &format!("initial-{}-{}-{}.section", key[0], key[1], key[2]), bytes)?}));
    }
    let edit_templates = [
        save(&root, "edit-0.section", &edits[0][&EDIT_KEY])?,
        save(&root, "edit-1.section", &edits[1][&EDIT_KEY])?,
    ];
    let environment = Environment {
        sea_level: 0.,
        ..Environment::default()
    }
    .validated()?;
    save_json(
        &root,
        "input.json",
        &json!({
            "version":3,"scene_update":"engine chunk_manager LOD0; PRT2 tickets; staged publication; section BLAS cache",
            "extent":EXTENT,"nonempty_sections":keys,"window_sections":1728,"quads":98304,
            "edit_section":EDIT_KEY,"edit":"one cube height 1.0/1.5; replace one complete section",
            "initial_templates":templates,"edit_templates":edit_templates,
            "template_format":"sx/sy/sz/block_count/blocks; PRT2 request IDs/header attached at response time",
            "capture_protocol":"PRT2; every requested section answered, including 1664 empty initial resources",
            "atlas":save(&root,"atlas.rgba8",&atlas)?,"atlas_extent":[16,16],
            "camera":{"origin":camera.origin,"forward":camera.forward,"right":camera.right,"up":camera.up},
            "environment":{"sun_direction":environment.sun_direction,"sea_level":environment.sea_level,"exposure_ev":environment.exposure_ev},
            "integrator":{"samples":1,"max_vertices":12,"seed":0,"max_dielectric_events":128,"rr":false,"ser":false},
            "rounds":ROUNDS,"steady_frames_per_round":FRAMES,"edits_per_round":EDITS,
            "boundary":"Host::render_options: command-pool creation, draw/encode, submit, queue idle, Flight completion; readback excluded",
            "scene_update_boundary":"manager_complete: plan+PRT2 packet response+apply+prepare+set_sections+commit/retire; all empty resources included; output file writes excluded",
            "comparison_boundary":"old direct 64-section apply is a different workload; steady image/integrator inputs retained",
            "steady_boundaries":"draw_complete_ns retains host draw/submit/complete only; manager_steady_ns includes real plan/prepare; frame_complete_ns includes manager and draw",
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
    let start = Instant::now();
    gpu.set_atlas(16, 16, &atlas)?;
    let initial_atlas_ns = ns(start);
    let initial_drive = settle(&mut scene, &mut gpu, &world, &initial)?;
    let initial_scene_complete_ns = initial_drive.gpu_scene_ns;
    let initial_prepare_ns = ns(start);
    let initial_scene_build = initial_drive.report();
    assert_eq!(initial_drive.requests, 1728);
    assert_eq!(scene.entries().len(), keys.len());
    assert_eq!(
        scene
            .entries()
            .map(|(_, _, mesh)| mesh.quads.len())
            .sum::<usize>(),
        98304
    );
    let mut capture_packets = Vec::new();
    for (index, bytes) in initial_drive.packets.iter().enumerate() {
        capture_packets.push(save(&root, &format!("initial-packet-{index}.prt2"), bytes)?);
    }
    save_json(
        &root,
        "initial-capture.json",
        &json!({"packets":capture_packets,"manager":initial_scene_build}),
    )?;
    let camera = world.relative(scene.anchor())?;
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
            let frame_started = Instant::now();
            let manager_started = Instant::now();
            assert!(
                scene.plan(&world, 64)?.is_empty(),
                "稳定窗口不应请求重新捕获"
            );
            assert!(!scene.prepare()?, "稳定窗口不应重新发布GPU场景");
            let manager_steady_ns = ns(manager_started);
            let start = Instant::now();
            host.render_options(&mut gpu, &image, &camera, environment, options)?;
            let draw_complete_ns = ns(start);
            let frame_complete_ns = ns(frame_started);
            record(
                &mut raw,
                &json!({"case":"steady","round":round,"frame":frame,
                "first_sample":frame,"samples":1,"draw_complete_ns":draw_complete_ns,
                "manager_steady_ns":manager_steady_ns,"frame_complete_ns":frame_complete_ns}),
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
    // 编辑同样请求真实刷新ticket；PRT2响应、空源与全部manager调度计入完整准备。
    for round in 0..ROUNDS {
        for edit in 0..EDITS {
            let variant = (edit + 1) % 2;
            let started = Instant::now();
            scene.invalidate_section(EDIT_KEY)?;
            let manager = settle(&mut scene, &mut gpu, &world, &edits[variant])?;
            let apply_ns = manager.apply_ns;
            let merge_ns = 0u64;
            let set_scene_complete_ns = manager.gpu_scene_ns;
            let prepare_complete_ns = ns(started);
            assert_eq!(manager.requests, 1);
            assert_eq!(manager.blas_count, 1);
            let stage = Instant::now();
            host.render_options(&mut gpu, &image, &camera, environment, options)?;
            let draw_complete_ns = ns(stage);
            let update_draw_complete_ns = ns(started);
            let mut packets = Vec::new();
            for (index, bytes) in manager.packets.iter().enumerate() {
                packets.push(save(
                    &root,
                    &format!("edit-{round}-{edit}-packet-{index}.prt2"),
                    bytes,
                )?);
            }
            record(
                &mut raw,
                &json!({"case":"single_section_edit","round":round,"edit":edit,
                "variant":variant,"first_sample":0,"samples":1,"apply_ns":apply_ns,
                "merge_ns":merge_ns,"set_scene_complete_ns":set_scene_complete_ns,
                "prepare_complete_ns":prepare_complete_ns,"draw_complete_ns":draw_complete_ns,
                "update_draw_complete_ns":update_draw_complete_ns,"manager":manager.report(),"packets":packets,"scene_build":scene_build_metrics(&gpu)}),
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
#[ignore = "真实Vulkan RT；manager刷新与anchor重定位保留未变BLAS"]
fn hardware_pt_section_update_reuses_unmodified_blas() -> Result {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = crate::scene::Scene::new()?;
    let keys = [[0, 0, 0], EDIT_KEY];
    let world = world_camera(scene_camera());
    settle(
        &mut scene,
        &mut gpu,
        &world,
        &sources(&keys, false, 0x00bf_d9a6, false),
    )?;
    let initial: Vec<_> = gpu
        .section_cache
        .entries()
        .iter()
        .map(|entry| entry.blas)
        .collect();
    scene.invalidate_section(EDIT_KEY)?;
    settle(
        &mut scene,
        &mut gpu,
        &world,
        &sources(&keys, true, 0x00bf_d9a6, false),
    )?;
    let changed: Vec<_> = gpu
        .section_cache
        .entries()
        .iter()
        .map(|entry| entry.blas)
        .collect();
    assert_eq!(initial[0], changed[0]);
    assert_ne!(initial[1], changed[1]);
    let moved = crate::scene::WorldCamera {
        origin: [world.origin[0] + 64.0, world.origin[1], world.origin[2]],
        ..world
    };
    settle(
        &mut scene,
        &mut gpu,
        &moved,
        &sources(&keys, true, 0x00bf_d9a6, false),
    )?;
    assert!(
        gpu.section_cache
            .entries()
            .iter()
            .map(|entry| entry.blas)
            .eq(changed)
    );
    let image = image(&mut gpu)?;
    let camera = world.relative(scene.anchor())?;
    let local = host.render(&mut gpu, &image, &camera)?;
    // 独立全局合批仅作结果oracle，所有被测增量准备均经过manager ticket。
    gpu.set_scene(&scene.merge())?;
    let reference = host.render(&mut gpu, &image, &camera)?;
    assert!(
        local
            .iter()
            .zip(&reference)
            .all(|(&a, &b)| a.abs_diff(b) <= 1),
        "manager局部BLAS实例与独立全局合批的宿主图像不等价"
    );
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
#[ignore = "真实Vulkan RT；manager属性刷新保持BLAS且改变宿主输出"]
fn hardware_pt_section_attributes_reuse_blas_and_change_output() -> Result {
    let host = Host::new()?;
    let mut gpu = host.gpu()?;
    gpu.set_atlas(16, 16, &atlas())?;
    let mut scene = crate::scene::Scene::new()?;
    let camera = Camera {
        origin: [8., 8., -12., 0.],
        forward: [0., 0., 1., 0.],
        right: [0.8, 0., 0., 0.],
        up: [0., 0.8, 0., 0.],
    };
    let world = world_camera(camera);
    settle(
        &mut scene,
        &mut gpu,
        &world,
        &sources(&[[0, 0, 0]], false, 0x00bf_d9a6, false),
    )?;
    let initial = gpu.section_cache.entries()[0].blas;
    let image = image(&mut gpu)?;
    let relative = world.relative(scene.anchor())?;
    let before = host.render(&mut gpu, &image, &relative)?;
    scene.invalidate_section([0, 0, 0])?;
    settle(
        &mut scene,
        &mut gpu,
        &world,
        &sources(&[[0, 0, 0]], false, 0x0030_e030, true),
    )?;
    assert_eq!(gpu.section_cache.entries()[0].blas, initial);
    let after = host.render(&mut gpu, &image, &relative)?;
    assert_ne!(before, after, "只更新tint/UV必须传播到实际场景属性");
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}

#[path = "scene_window_tests.rs"]
mod window_tests;
