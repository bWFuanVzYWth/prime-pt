//! Minecraft 场景适配与公共 PT 算法编译为 Slang 原生 SPIR-V。

use simple_engine::shader_build::{Recipe, Target, build};
use simple_engine::shader_library;
use std::path::{Path, PathBuf};

fn relative_path(path: &Path, base: &Path) -> Result<PathBuf, &'static str> {
    let common = base
        .ancestors()
        .find(|ancestor| path.starts_with(ancestor))
        .ok_or("开发 shader 路径与应用源码须位于同一文件系统根")?;
    let mut relative = PathBuf::new();
    for _ in base
        .strip_prefix(common)
        .map_err(|_| "开发源码基目录无效")?
        .components()
    {
        relative.push("..");
    }
    relative.push(
        path.strip_prefix(common)
            .map_err(|_| "引擎 shader 目录无效")?,
    );
    Ok(relative)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")?;
    let relative = relative_path(Path::new(shader_library::root()), Path::new(&manifest))?;
    println!(
        "cargo::rustc-env=PRIME_ENGINE_SHADERS_RELATIVE={}",
        relative.display()
    );
    let exposure_source = format!("{}/display/exposure_compute.slang", shader_library::root());
    let stages = [
        ("prefix_main", "raygeneration", "prefix.spv"),
        ("render_main", "raygeneration", "render.spv"),
        ("primary_miss", "miss", "miss.spv"),
        ("primary_anyhit", "anyhit", "anyhit.spv"),
        ("primary_closesthit", "closesthit", "closesthit.spv"),
    ];
    let mut recipes: Vec<_> = stages
        .iter()
        .map(|(entry, stage, output)| Recipe {
            source: "shaders/primary.slang",
            entry,
            stage,
            output,
            target: Target::Spirv("spirv_1_4"),
            args: if *stage == "closesthit" {
                &[
                    "-fp-mode",
                    "precise",
                    "-capability",
                    "spvRayTracingPositionFetchKHR",
                ]
            } else {
                &["-fp-mode", "precise"]
            },
        })
        .collect();
    for (source, entry, stage, output) in [
        (
            "shaders/restir.slang",
            "restir_initial",
            "raygeneration",
            "restir-initial.spv",
        ),
        (
            "shaders/restir.slang",
            "restir_temporal_direction",
            "raygeneration",
            "restir-temporal-direction.spv",
        ),
        (
            "shaders/restir.slang",
            "restir_spatial_direction",
            "raygeneration",
            "restir-spatial-direction.spv",
        ),
        (
            "shaders/restir.slang",
            "restir_merge",
            "compute",
            "restir-merge.spv",
        ),
        (
            "shaders/restir.slang",
            "restir_temporal_combine",
            "compute",
            "restir-temporal-combine.spv",
        ),
        (
            "shaders/restir.slang",
            "restir_spatial_work",
            "compute",
            "restir-spatial-work.spv",
        ),
    ] {
        recipes.push(Recipe {
            source,
            entry,
            stage,
            output,
            target: Target::Spirv("spirv_1_4"),
            args: &["-fp-mode", "precise"],
        });
    }
    for (entry, output) in [
        ("restir_initial", "restir-initial-ser.spv"),
        (
            "restir_temporal_direction",
            "restir-temporal-direction-ser.spv",
        ),
        (
            "restir_spatial_direction",
            "restir-spatial-direction-ser.spv",
        ),
    ] {
        recipes.push(Recipe {
            source: "shaders/restir.slang",
            entry,
            stage: "raygeneration",
            output,
            target: Target::Spirv("spirv_1_4"),
            args: &[
                "-fp-mode",
                "precise",
                "-DPRIMEPT_SER=1",
                "-capability",
                "spvShaderInvocationReorderEXT",
            ],
        });
    }
    for (output, args) in [
        (
            "anyhit-cutout.spv",
            &["-fp-mode", "precise", "-DPRIMEPT_COVERAGE_MASK=8"][..],
        ),
        (
            "anyhit-front.spv",
            &["-fp-mode", "precise", "-DPRIMEPT_COVERAGE_MASK=32"][..],
        ),
    ] {
        recipes.push(Recipe {
            source: "shaders/primary.slang",
            entry: "primary_anyhit",
            stage: "anyhit",
            output,
            target: Target::Spirv("spirv_1_4"),
            args,
        });
    }
    recipes.push(Recipe {
        source: "shaders/primary.slang",
        entry: "primary_closesthit",
        stage: "closesthit",
        output: "closesthit-flat.spv",
        target: Target::Spirv("spirv_1_4"),
        args: &[
            "-fp-mode",
            "precise",
            "-capability",
            "spvRayTracingPositionFetchKHR",
            "-DPRIMEPT_AUTHORED_NORMAL=0",
        ],
    });
    recipes.push(Recipe {
        source: "shaders/primary.slang",
        entry: "render_main",
        stage: "raygeneration",
        output: "render-ser.spv",
        target: Target::Spirv("spirv_1_4"),
        args: &[
            "-fp-mode",
            "precise",
            "-DPRIMEPT_SER=1",
            "-capability",
            "spvShaderInvocationReorderEXT",
        ],
    });
    for (source, entry, output) in [
        (
            "shaders/light_test.slang",
            "light_probe_main",
            "light_probe.spv",
        ),
        (
            "shaders/light_test.slang",
            "region_primitive_probe_main",
            "region_primitive_probe.spv",
        ),
        ("shaders/post.slang", "post_main", "post.spv"),
        (
            "shaders/restir_probe.slang",
            "restir_cv_neighbors",
            "restir_cv_neighbors.spv",
        ),
        (
            "shaders/restir_probe.slang",
            "restir_donor_probe",
            "restir_donor_probe.spv",
        ),
        (
            "shaders/restir_probe.slang",
            "restir_identity_probe",
            "restir_identity_probe.spv",
        ),
        ("shaders/present.slang", "present_main", "present.spv"),
        ("shaders/rr.slang", "pack_main", "rr_pack.spv"),
        ("shaders/rr.slang", "present_main", "rr_present.spv"),
        ("shaders/rr.slang", "fallback_main", "rr_fallback.spv"),
        (
            "shaders/visibility_test.slang",
            "visibility_main",
            "visibility.spv",
        ),
    ] {
        recipes.push(Recipe {
            source,
            entry,
            stage: "compute",
            output,
            target: Target::Spirv("spirv_1_4"),
            args: &["-fp-mode", "precise"],
        });
    }
    recipes.push(Recipe {
        source: "shaders/visibility_test.slang",
        entry: "reference_main",
        stage: "raygeneration",
        output: "visibility_reference.spv",
        target: Target::Spirv("spirv_1_4"),
        args: &["-fp-mode", "precise"],
    });
    for (entry, output, args) in [
        (
            "histogram_main",
            "exposure.buffer.spv",
            &["-DEXPOSURE_TEXTURE=0"][..],
        ),
        (
            "histogram_main",
            "exposure.texture.spv",
            &["-DEXPOSURE_TEXTURE=1"][..],
        ),
        ("update_main", "exposure.update.spv", &[][..]),
    ] {
        recipes.push(Recipe {
            source: &exposure_source,
            entry,
            stage: "compute",
            output,
            target: Target::Spirv("spirv_1_4"),
            args,
        });
    }
    build(&recipes, &["shaders", shader_library::root()], &["src"])?;
    Ok(())
}
