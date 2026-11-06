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
                    {"primary.slang", "shade_opaque", "1", "callable"},
                    {"pt_hit.slang", "shade_hit", "1", "closesthit"},
                    {"pt_hit.slang", "render_fused", "1", "raygeneration"},
                    {"restir_material.slang", "restir_initial_flow", "1", "callable"},
                    {"restir_material.slang", "restir_initial_flow", "0", "callable"},
                    {"restir_material.slang", "restir_replay_flow", "1", "callable"},
                    {"restir_material.slang", "restir_evaluate_flow", "0", "callable"}}) {
            var builder = new ProcessBuilder(root.resolve("slang/slangc.exe").toString(), root.resolve("shaders/prime/" + stage[0]).toString(),
                    "-entry", stage[1], "-stage", stage[3], "-I", root.resolve("shaders/prime").toString(),
                    "-I", root.resolve("shaders/engine").toString(), "-std", "2026", "-target", "spirv",
                    "-profile", "spirv_1_4", "-emit-spirv-directly", "-fvk-use-entrypoint-name", "-O3", "-g0",
                    "-fp-mode", "precise", "-DOPENPBR_FLOW=0", "-DOPENPBR_VARIANT=0", "-DPRIMEPT_LIGHT_MASK=1",
                    "-DPRIMEPT_RESTIR_SAMPLE=" + stage[2],
                    "-DPRIMEPT_COVERAGE_MASK=0", "-o", output.toString());
            if (stage[3].equals("raygeneration")) {
                builder.command().addAll(java.util.List.of("-capability", "spvShaderInvocationReorderEXT",
                        "-capability", "spvRayTracingPositionFetchKHR", "-DPT_HIT_SER=1"));
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
