//! 显式Release CPU基线，输入由生产PRM/PRT3 writer fixture产生；不启动游戏或GPU。
use prime_minecraft::fixtures::{self, Pattern};
use prime_minecraft::{
    Catalog, CompiledSection, Neighbor, Neighborhood, TintPatch, compile_section, decode_sections,
};
use prime_scene::{Mesh, WorldCamera};
use prime_world::Scene;
use serde_json::{Value, json};
use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    hint::black_box,
    io::Write,
    path::PathBuf,
    time::Instant,
};
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;
const WARMUP: usize = 3;
const SAMPLES: usize = 16;

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn write_json(file: &mut File, value: &Value) -> Result {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}
fn camera() -> WorldCamera {
    WorldCamera {
        origin: [0.; 3],
        forward: [0., 0., 1., 0.],
        right: [1., 0., 0., 0.],
        up: [0., 1., 0., 0.],
    }
}
fn measure(
    file: &mut File,
    name: &str,
    workers: usize,
    mut run: impl FnMut() -> Result<Value>,
) -> Result {
    for sample in 0..WARMUP + SAMPLES {
        let started = Instant::now();
        let result = run()?;
        let elapsed = started.elapsed().as_nanos() as u64;
        write_json(
            file,
            &json!({"case":name,"workers":workers,"sample":sample,"warmup":sample<WARMUP,"wall_ns":elapsed,"result":result}),
        )?;
    }
    Ok(())
}
fn consume(output: CompiledSection) -> Value {
    let quads = output.mesh.quads.len();
    let tints = output.tints.len();
    let checksum = output
        .mesh
        .quads
        .first()
        .map_or(0, |quad| quad.positions()[0][0].to_bits());
    black_box(&output);
    drop(output);
    json!({"quads":quads,"tint_patches":tints,"checksum":checksum})
}
fn neighborhood<'a>(source: &'a [prime_minecraft::SectionSource]) -> Neighborhood<'a> {
    Neighborhood {
        key: [0; 3],
        generation: 1,
        sections: std::array::from_fn(|index| {
            let key = [index % 3, index / 9, (index / 3) % 3].map(|v| v as i32 - 1);
            let source = source
                .iter()
                .find(|source| source.key == key)
                .expect("固定27邻域");
            source
                .storage
                .as_ref()
                .map_or(Neighbor::Unavailable, Neighbor::Available)
        }),
    }
}
fn neighborhood_packet(pattern: Pattern) -> Vec<u8> {
    let mut records = Vec::new();
    for y in -1..=1 {
        for z in -1..=1 {
            for x in -1..=1 {
                let key = [x, y, z];
                records.push((
                    records.len() as u64 + 1,
                    key,
                    if key == [0; 3] { pattern } else { Pattern::Air },
                ));
            }
        }
    }
    fixtures::section_packet(&records)
}
fn clone_output(output: &CompiledSection) -> CompiledSection {
    CompiledSection {
        mesh: Mesh {
            quads: output.mesh.quads.clone(),
        },
        tints: output
            .tints
            .iter()
            .map(|patch| TintPatch {
                quad: patch.quad,
                request: patch.request,
            })
            .collect(),
    }
}
fn drive(
    scene: &mut Scene,
    catalog: &Catalog,
    pattern: Pattern,
    whole_region: bool,
) -> Result<Value> {
    let mut requests = 0;
    let mut queries = 0;
    let mut steps = 0;
    for _ in 0..20_000 {
        steps += 1;
        let batch = scene.plan(&camera(), 0, 0, 4, 64)?.to_vec();
        requests += batch.len();
        if !batch.is_empty() {
            let records: Vec<_> = batch
                .iter()
                .map(|request| {
                    (
                        request.id,
                        request.section,
                        if request.section == [0; 3]
                            || whole_region && request.section.iter().all(|v| (0..4).contains(v))
                        {
                            pattern
                        } else {
                            Pattern::Air
                        },
                    )
                })
                .collect();
            scene.apply_compact(&fixtures::section_packet(&records), catalog)?;
        } else {
            scene.compile_sources(catalog)?;
        }
        let colors = vec![0xff71_a94b; scene.tint_requests().len()];
        queries += colors.len();
        if !colors.is_empty() {
            scene.apply_tints(&colors)?;
        }
        if scene.prepare()? {
            black_box(
                scene
                    .render_regions()
                    .map(|region| region.mesh.len())
                    .sum::<usize>(),
            );
            // 无GPU消费者；此case只测真实CPU提交，不能推断AS/上传/游戏帧率。
            scene.commit()?;
        }
        if !scene.pending() {
            return Ok(
                json!({"steps":steps,"capture_requests":requests,"unique_tint_queries":queries,
                "quads":scene.render_regions().map(|region|region.mesh.len()).sum::<usize>(),
                "pressure_kind":scene.pressure().kind as u32}),
            );
        }
    }
    Err("world benchmark未收敛".into())
}
fn tint_result(
    world: &mut prime_minecraft::tint::TintWorld,
    resources: &prime_minecraft::tint::TintResources,
    requests: &[prime_minecraft::TintRequest],
    radius: u32,
) -> Result<Value> {
    let plan = world.plan(black_box(requests), resources, radius)?;
    let hits = plan.cached_colors();
    let source = plan.source_samples();
    let raw = plan.biome_sample_count();
    let host = plan.host_requests().len();
    let colors = world.finish(plan, resources, &vec![0; raw], &vec![0xffaa_bbcc; host])?;
    let checksum = colors
        .iter()
        .fold(0u64, |sum, color| sum.wrapping_add(u64::from(*color)));
    black_box(&colors);
    drop(colors);
    Ok(
        json!({"cached_colors":hits,"source_samples":source,"biome_quarts":raw,"host_callbacks":host,"checksum":checksum}),
    )
}
fn vanilla_tint_cases(raw: &mut File, output: &std::path::Path) -> Result {
    use prime_minecraft::tint::fixtures as tint_fixture;
    let resources = tint_fixture::resources()?;
    let requests = tint_fixture::requests();
    fs::write(
        output.join("tint-resources.ptr1"),
        tint_fixture::resource_bytes(),
    )?;
    fs::write(output.join("tint-world.pbw1"), tint_fixture::world_bytes())?;
    let mut wire = Vec::new();
    for request in &requests {
        for p in request.position {
            wire.extend_from_slice(&p.to_le_bytes());
        }
        wire.extend_from_slice(&request.state.to_le_bytes());
        wire.extend_from_slice(&request.slot.to_le_bytes());
        wire.extend_from_slice(&0u32.to_le_bytes());
    }
    fs::write(output.join("tint-requests.tint24"), &wire)?;
    for radius in [0, 7] {
        for sample in 0..WARMUP + SAMPLES {
            let mut world = tint_fixture::world(&resources)?;
            let started = Instant::now();
            let result = tint_result(&mut world, &resources, &requests, radius)?;
            let elapsed = started.elapsed().as_nanos() as u64;
            write_json(
                raw,
                &json!({"case":format!("vanilla_tint/cold_radius{radius}"),"workers":1,"sample":sample,"warmup":sample<WARMUP,"wall_ns":elapsed,"result":result}),
            )?;
        }
        let mut world = tint_fixture::world(&resources)?;
        tint_result(&mut world, &resources, &requests, radius)?;
        measure(raw, &format!("vanilla_tint/warm_radius{radius}"), 1, || {
            tint_result(&mut world, &resources, &requests, radius)
        })?;
        measure(
            raw,
            &format!("vanilla_tint/recompute_radius{radius}"),
            1,
            || {
                world.invalidate(true, &std::collections::HashSet::new());
                tint_result(&mut world, &resources, &requests, radius)
            },
        )?;
    }
    let mut world = tint_fixture::world(&resources)?;
    let mixed = [
        prime_minecraft::TintRequest {
            position: [0, 64, 0],
            state: 3,
            slot: 0,
        },
        prime_minecraft::TintRequest {
            position: [1, 64, 0],
            state: 4,
            slot: 0,
        },
        prime_minecraft::TintRequest {
            position: [2, 64, 0],
            state: 5,
            slot: 0,
        },
    ];
    measure(raw, "vanilla_tint/mixed_custom", 1, || {
        tint_result(&mut world, &resources, &mixed, 0)
    })?;
    let inputs = [
        ("tint-resources.ptr1", tint_fixture::resource_bytes()),
        ("tint-world.pbw1", tint_fixture::world_bytes()),
        ("tint-requests.tint24", wire),
    ];
    fs::write(
        output.join("tint-inputs.json"),
        serde_json::to_vec_pretty(
            &json!({"inputs":inputs.iter().map(|(name,bytes)|json!({"file":name,"bytes":bytes.len(),"blake3":hash(bytes)})).collect::<Vec<_>>(),"radius":[0,7],"raw_quart_palette_index":0,"custom_argb":"ffaabbcc","mixed":[{"position":[0,64,0],"state":3,"slot":0},{"position":[1,64,0],"state":4,"slot":0},{"position":[2,64,0],"state":5,"slot":0}]}),
        )?,
    )?;
    Ok(())
}

fn main() -> Result {
    if cfg!(debug_assertions) {
        return Err("CPU测量必须使用Release/bench profile".into());
    }
    let output = PathBuf::from(
        std::env::var_os("PRIME_WORLD_BENCH_OUTPUT")
            .ok_or("设置PRIME_WORLD_BENCH_OUTPUT为新目录")?,
    );
    fs::create_dir(&output)?;
    let mut raw = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output.join("samples.jsonl"))?;
    let catalog = fixtures::catalog()?;
    let resources = fixtures::resource_packet();
    fs::write(output.join("resources.prm1"), &resources)?;
    let cases = [
        ("air", Pattern::Air),
        ("model_dense", Pattern::Dense),
        ("model_sparse", Pattern::Sparse),
        ("fluid", Pattern::Fluid),
        ("tint_generate", Pattern::Tinted),
    ];
    let mut inputs = Vec::new();
    for (name, pattern) in cases {
        let bytes = neighborhood_packet(pattern);
        fs::write(output.join(format!("{name}.prt3")), &bytes)?;
        inputs.push(json!({"case":name,"bytes":bytes.len(),"blake3":hash(&bytes)}));
        measure(&mut raw, &format!("decode/{name}"), 1, || {
            let source = decode_sections(black_box(&bytes))?;
            let payload = source
                .iter()
                .filter_map(|value| value.storage.as_ref())
                .map(|value| value.source_bytes())
                .sum::<usize>();
            black_box(&source);
            drop(source);
            Ok(json!({"source_bytes":payload}))
        })?;
        let source = decode_sections(&bytes)?;
        let neighborhood = neighborhood(&source);
        measure(&mut raw, &format!("section/{name}"), 1, || {
            Ok(consume(compile_section(
                black_box(&catalog),
                black_box(&neighborhood),
            )?))
        })?;
        if name == "tint_generate" {
            let template = compile_section(&catalog, &neighborhood)?;
            let colors = vec![0xff71_a94b; template.tints.len()];
            // 输入复制属于setup，独立计时apply及输出消费/释放；原始样本保留边界。
            for sample in 0..WARMUP + SAMPLES {
                let mut output = clone_output(&template);
                let started = Instant::now();
                output.apply_tints(black_box(&colors))?;
                let result = consume(output);
                let elapsed = started.elapsed().as_nanos() as u64;
                write_json(
                    &mut raw,
                    &json!({"case":"tint/apply","workers":1,"sample":sample,"warmup":sample<WARMUP,"wall_ns":elapsed,"result":result}),
                )?;
            }
        }
    }
    for workers in [1, 8] {
        // 私有池和Catalog创建在计时外；每次调用真实plan/PRT3 producer/compile/tint/commit。
        for sample in 0..WARMUP + SAMPLES {
            let mut scene = Scene::with_threads(workers)?;
            scene.source_generation(catalog.generation())?;
            let started = Instant::now();
            let result = drive(&mut scene, &catalog, Pattern::Tinted, false)?;
            let elapsed = started.elapsed().as_nanos() as u64;
            write_json(
                &mut raw,
                &json!({"case":"world/initial_tinted","workers":workers,"sample":sample,"warmup":sample<WARMUP,"wall_ns":elapsed,"result":result}),
            )?;
            scene.invalidate_tints();
            let started = Instant::now();
            let result = drive(&mut scene, &catalog, Pattern::Tinted, false)?;
            let elapsed = started.elapsed().as_nanos() as u64;
            write_json(
                &mut raw,
                &json!({"case":"world/tint_recompute","workers":workers,"sample":sample,"warmup":sample<WARMUP,"wall_ns":elapsed,"result":result}),
            )?;
            scene.invalidate_section([0; 3])?;
            let started = Instant::now();
            let result = drive(&mut scene, &catalog, Pattern::Sparse, false)?;
            let elapsed = started.elapsed().as_nanos() as u64;
            write_json(
                &mut raw,
                &json!({"case":"world/section_refresh","workers":workers,"sample":sample,"warmup":sample<WARMUP,"wall_ns":elapsed,"result":result}),
            )?;
        }
    }
    for workers in [1, 8] {
        for sample in 0..WARMUP + SAMPLES {
            let mut scene = Scene::with_threads(workers)?;
            scene.source_generation(catalog.generation())?;
            let started = Instant::now();
            let result = drive(&mut scene, &catalog, Pattern::Dense, true)?;
            write_json(
                &mut raw,
                &json!({"case":"world/initial_dense_region","workers":workers,"sample":sample,"warmup":sample<WARMUP,"wall_ns":started.elapsed().as_nanos() as u64,"result":result}),
            )?;
        }
    }
    vanilla_tint_cases(&mut raw, &output)?;
    // 新增case追加在所有已有case之后，不把它冒充before已有覆盖。
    for workers in [1, 8] {
        for sample in 0..WARMUP + SAMPLES {
            let mut scene = Scene::with_threads(workers)?;
            scene.source_generation(catalog.generation())?;
            let started = Instant::now();
            let result = drive(&mut scene, &catalog, Pattern::Air, true)?;
            write_json(
                &mut raw,
                &json!({"case":"world/initial_air_region","workers":workers,"sample":sample,"warmup":sample<WARMUP,"wall_ns":started.elapsed().as_nanos() as u64,"result":result}),
            )?;
        }
    }
    let metadata = json!({"version":2,"profile":"release","warmup":WARMUP,"samples":SAMPLES,"workers":[1,8],"resources_blake3":hash(&resources),"inputs":inputs,
        "executable":std::env::current_exe()?.to_string_lossy(),"executable_blake3":hash(&fs::read(std::env::current_exe()?)?),
        "boundaries":{"decode":"production PRT3 decode+owned source consumption/drop; wire preparation excluded","section":"production serial 4slab section compiler+mesh/tint consumption/drop; Catalog/halo setup excluded",
        "tint_apply":"production apply_tints+output consumption/drop; producer template copy excluded","vanilla_tint":"real TintWorld plan+fixture raw biome/custom response allocation+finish+color consumption/drop; world/resource setup/drop outside timer; recompute includes invalidate","world":"owned pool setup excluded; includes plan+Rust fixture PRT3 writer+owned decode+4slab fork/join+section merge+unique tint callback response+CPU prepare/commit; no GPU"},
        "omissions":["Minecraft/Java capture and real host tint callback cost","GPU upload/AS/render/present","no old equivalent source-pipeline baseline; first API baseline only"]});
    fs::write(
        output.join("input.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    Ok(())
}
