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
    let display_root = simple_engine::display::shader_root();
    let display_relative = relative_path(Path::new(display_root), Path::new(&manifest))?;
    println!(
        "cargo::rustc-env=PRIME_DISPLAY_SHADERS_RELATIVE={}",
        display_relative.display()
    );
    let exposure_source = format!("{display_root}/display/exposure_compute.slang");
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
        (
            "shaders/light_test.slang",
            "triangle_identity_probe_main",
            "triangle_identity_probe.spv",
        ),
        ("shaders/post.slang", "post_main", "post.spv"),
        ("shaders/post.slang", "post_nrd", "post_nrd.spv"),
        (
            "shaders/shadow_columns.slang",
            "shadow_columns",
            "shadow_columns.spv",
        ),
        ("shaders/present.slang", "present_main", "present.spv"),
        ("shaders/rr.slang", "pack_main", "rr_pack.spv"),
        ("shaders/rr.slang", "resolve_raw", "resolve_raw.spv"),
        ("shaders/rr.slang", "resolve_rr", "resolve_rr.spv"),
        ("shaders/rr.slang", "resolve_scaled", "resolve_scaled.spv"),
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
    for (entry, output) in [
        ("histogram_main", "exposure.buffer.spv"),
        ("histogram_texture_main", "exposure.texture.spv"),
        ("update_main", "exposure.update.spv"),
    ] {
        recipes.push(Recipe {
            source: &exposure_source,
            entry,
            stage: "compute",
            output,
            target: Target::Spirv("spirv_1_4"),
            args: &[],
        });
    }
    // 有限阶段策略在资源使用前编译，录制期间只选取已准备的程序。
    for (source, entry, output) in [
        ("shaders/post.slang", "post_plain_1", "post_plain_1.spv"),
        ("shaders/post.slang", "post_plain_2", "post_plain_2.spv"),
        (
            "shaders/post.slang",
            "post_clear_random_1",
            "post_clear_random_1.spv",
        ),
        (
            "shaders/post.slang",
            "post_clear_random_2",
            "post_clear_random_2.spv",
        ),
        (
            "shaders/post.slang",
            "post_clear_jittered_1",
            "post_clear_jittered_1.spv",
        ),
        (
            "shaders/post.slang",
            "post_clear_jittered_2",
            "post_clear_jittered_2.spv",
        ),
        (
            "shaders/post.slang",
            "post_shadowed_random_1",
            "post_shadowed_random_1.spv",
        ),
        (
            "shaders/post.slang",
            "post_shadowed_random_2",
            "post_shadowed_random_2.spv",
        ),
        (
            "shaders/post.slang",
            "post_shadowed_jittered_1",
            "post_shadowed_jittered_1.spv",
        ),
        (
            "shaders/post.slang",
            "post_shadowed_jittered_2",
            "post_shadowed_jittered_2.spv",
        ),
        (
            "shaders/present.slang",
            "present_manual",
            "present_present_manual.spv",
        ),
        (
            "shaders/present.slang",
            "present_automatic",
            "present_present_automatic.spv",
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
    build(
        &recipes,
        &["shaders", shader_library::root(), display_root],
        &["src"],
    )?;
    Ok(())
}
