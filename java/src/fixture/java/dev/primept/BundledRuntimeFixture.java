package dev.primept;

import java.lang.foreign.Arena;
import java.lang.foreign.SymbolLookup;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.concurrent.TimeUnit;

/** 只从发行 JAR 提取依赖、打开 DLL，并编译实际运行时材质与主射线程序。 */
final class BundledRuntimeFixture {
    public static void main(String[] arguments) throws Throwable {
        require(System.getProperty("primept.native") == null, "发行夹具必须使用内置依赖");
        var cache = Path.of(arguments[0]);
        var bundle = NativeRuntime.extract(cache);
        var root = bundle.library().getParent();
        var source = root.resolve("shaders/prime/primary.slang");
        var original = Files.readAllBytes(source);
        Files.writeString(source, "损坏的已提取源码");
        require(NativeRuntime.extract(cache).equals(bundle) && Arrays.equals(original, Files.readAllBytes(source)),
                "复用缓存时须恢复损坏文件");

        try (var arena = Arena.ofConfined()) {
            var symbols = NativeRuntime.open(bundle, arena);
            require(symbols.find("prime_create").isPresent(), "JAR 内原生 DLL 缺少会话入口");
            require(symbols.find("prime_set_rendering").isPresent(), "JAR 内原生 DLL 缺少渲染模式入口");
            require(symbols.find("prime_set_render_tuning").isPresent(), "JAR 内原生 DLL 缺少采样设置入口");
            require(SymbolLookup.libraryLookup(root.resolve("sl.interposer.dll"), arena).find("slInit").isPresent(),
                    "JAR 内 Streamline 缺少初始化入口");
        }

        var output = Files.createTempFile(cache, "callable-", ".spv");
        try {
            for (var stage : new String[][] {
                    {"primary.slang", "shade_opaque", "131", "0", "callable"},
                    {"pt_hit.slang", "shade_hit", "131", "0", "closesthit"},
                    {"primary_material.slang", "shade_primary", "131", "0", "callable"},
                    {"pt_hit.slang", "render_fused", "0", "0xffffffffu", "raygeneration"},
                    {"pt_hit.slang", "render_fused", "0", "0xffffffffu", "raygeneration", "0"},
                    {"guide_material.slang", "guide_primary", "2255", "11", "callable"},
                    {"guide_material.slang", "guide_hit", "2255", "11", "closesthit"}}) {
            var builder = new ProcessBuilder(root.resolve("slang/slangc.exe").toString(), root.resolve("shaders/prime/" + stage[0]).toString(),
                    "-entry", stage[1], "-stage", stage[4], "-I", root.resolve("shaders/prime").toString(),
                    "-I", root.resolve("shaders/engine").toString(), "-std", "2026", "-target", "spirv",
                    "-profile", "spirv_1_4", "-emit-spirv-directly", "-fvk-use-entrypoint-name", "-O3", "-g0",
                    "-fp-mode", "precise", "-DOPENPBR_FLOW=" + stage[2], "-DOPENPBR_VARIANT=" + stage[3],
                    "-DPRIMEPT_LIGHT_MASK=3", "-DPRIMEPT_EMITTER_CAPABILITIES=7",
                    "-DPRIMEPT_COVERAGE_MASK=0", "-o", output.toString());
            if (stage[4].equals("closesthit")) {
                builder.command().addAll(java.util.List.of("-capability", "spvRayTracingPositionFetchKHR"));
            }
            if (stage.length > 5) {
                builder.command().add("-DPRIMEPT_RECONSTRUCTION=" + stage[5]);
            }
            if (stage[4].equals("raygeneration")) {
                builder.command().addAll(java.util.List.of("-capability", "spvShaderInvocationReorderEXT",
                        "-capability", "spvRayTracingPositionFetchKHR", "-DPRIMEPT_SER=1"));
            }
            builder.directory(cache.toFile());
            builder.environment().remove("SLANGC");
            builder.environment().remove("VULKAN_SDK");
            builder.environment().put("PATH", Path.of(System.getenv("SystemRoot"), "System32").toString());
            builder.environment().put("SLANG_RUN_SPIRV_VALIDATION", "1");
            var compiler = builder.inheritIO().start();
            if (!compiler.waitFor(60, TimeUnit.SECONDS)) {
                compiler.destroyForcibly().waitFor();
                throw new AssertionError("发行包内 Slang 编译超时");
            }
            require(compiler.exitValue() == 0 && Files.size(output) > 20, "发行包内 Slang 未生成对应阶段 SPIR-V");
            }
        } finally {
            Files.deleteIfExists(output);
        }
        System.out.println("单 JAR 发行夹具通过：校验解包、Streamline 与运行时材质/主射线程序");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
