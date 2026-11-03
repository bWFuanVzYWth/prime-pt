//! Minecraft 场景适配与公共 PT 算法编译为 Slang 原生 SPIR-V。

use shader_build::{Recipe, Target, build};

fn main() -> Result<(), shader_build::Error> {
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
        ("shaders/post.slang", "post_main", "post.spv"),
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
            source: "../../../../shaders/display/exposure_compute.slang",
            entry,
            stage: "compute",
            output,
            target: Target::Spirv("spirv_1_4"),
            args,
        });
    }
    build(&recipes, &["shaders", "../../../../shaders"], &["src"])
}
