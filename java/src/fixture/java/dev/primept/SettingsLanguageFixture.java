package dev.primept;

import java.lang.reflect.Proxy;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Optional;
import net.fabricmc.fabric.api.resource.v1.pack.PackActivationType;
import net.fabricmc.fabric.impl.resource.pack.ModNioPackResources;
import net.fabricmc.loader.api.ModContainer;
import net.fabricmc.loader.impl.metadata.DependencyOverrides;
import net.fabricmc.loader.impl.metadata.ModMetadataParser;
import net.fabricmc.loader.impl.metadata.VersionOverrides;
import net.minecraft.SharedConstants;
import net.minecraft.client.Options;
import net.minecraft.client.gui.components.CycleButton;
import net.minecraft.client.resources.language.ClientLanguage;
import net.minecraft.locale.Language;
import net.minecraft.network.chat.CommonComponents;
import net.minecraft.network.chat.Component;
import net.minecraft.server.packs.PackLocationInfo;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.PathPackResources;
import net.minecraft.server.packs.VanillaPackResourcesBuilder;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;

/** 用发布 JAR、Fabric 资源包和原版语言栈验证实际设置文本。 */
final class SettingsLanguageFixture {
    public static void main(String[] arguments) throws Exception {
        SharedConstants.tryDetectVersion();
        var scratch = Files.createTempDirectory("prime-settings-lang");
        var assets = scratch.resolve("assets");
        var namespace = assets.resolve("prime_pt");
        var lang = namespace.resolve("lang");
        var override = lang.resolve("zh_cn.json");
        var metadataRoot = scratch.resolve("metadata");
        var packMetadata = metadataRoot.resolve("pack.mcmeta");
        try (var zip = FileSystems.newFileSystem(Path.of(arguments[0]))) {
            var jarRoot = zip.getPath("/");
            var vanilla = new VanillaPackResourcesBuilder().pushJarResources().exposeNamespace("minecraft")
                    .build(location("vanilla")).fullResources();
            // 无 Knot 的夹具没有 Fabric class tweaker。只补一份无过滤器的元数据，语言仍取自发布 JAR。
            Files.createDirectory(metadataRoot);
            Files.writeString(packMetadata, "{}");
            try (var metadataFile = Files.newInputStream(jarRoot.resolve("fabric.mod.json"))) {
                var metadata = ModMetadataParser.parseMetadata(metadataFile, arguments[0], List.of(),
                        new VersionOverrides(), new DependencyOverrides(scratch), false);
                require(metadata.getDependencies().stream().anyMatch(
                        dependency -> dependency.getModId().equals("fabric-resource-loader-v1")), "发布元数据要求 Fabric 资源加载器");
                require(metadata.getJars().stream().map(entry -> entry.getFile())
                        .anyMatch(file -> file.startsWith("META-INF/jars/fabric-resource-loader-v1-")
                                && Files.isRegularFile(jarRoot.resolve(file))), "发布 JAR 内嵌资源加载器");
                var mod = (ModContainer) Proxy.newProxyInstance(ModContainer.class.getClassLoader(),
                        new Class<?>[] {ModContainer.class}, (object, method, args) -> switch (method.getName()) {
                            case "getRootPaths" -> List.of(jarRoot, metadataRoot);
                            case "getMetadata" -> metadata;
                            default -> throw new UnsupportedOperationException(method.getName());
                        });
                var pack = ModNioPackResources.create(metadata.getId(), mod, null,
                        PackType.CLIENT_RESOURCES, PackActivationType.ALWAYS_ENABLED, true);
                require(pack != null, "Fabric 识别发布 JAR 的 assets 命名空间");
                try (var resources = new MultiPackResourceManager(PackType.CLIENT_RESOURCES, List.of(vanilla, pack))) {
                    Language.inject(ClientLanguage.loadFrom(resources, List.of("en_us"), false));
                    requireText("render", "PT rendering");
                    requireText("restir_cv", "ReSTCV color estimate");
                    requireRenderingHierarchy("Prime PT", "Rendering mode", "Offline snapshot", "Realtime renderer");
                    requireWidgetText("Samples per frame", "DLSS quality", "Performance (2×)");
                    requireAutoExposure("Auto exposure strength");
                    requireLighting("Sun intensity", "Block emission intensity");
                    requireStartupNotice("Prime PT unavailable", "OpenGL is active.", "GPU or driver lacks:");
                    Language.inject(ClientLanguage.loadFrom(resources, List.of("en_us", "zh_cn"), false));
                    requireText("render", "PT 渲染");
                    requireText("restir_cv", "ReSTCV 颜色估计");
                    requireRenderingHierarchy("Prime PT 开关", "渲染模式", "离线快照", "实时渲染器");
                    requireWidgetText("每帧采样数", "DLSS 质量", "性能（2×）");
                    requireAutoExposure("自动曝光强度");
                    requireLighting("太阳强度", "方块发光强度");
                    requireStartupNotice("Prime PT 不可用", "当前使用 OpenGL。", "显卡或驱动缺少：");
                }

                Files.createDirectories(lang);
                Files.writeString(override, "{\"primept.settings.render\":\"资源包覆盖\"}");
                var userPack = new PathPackResources(location("fixture"), scratch);
                try (var resources = new MultiPackResourceManager(PackType.CLIENT_RESOURCES, List.of(vanilla, pack, userPack))) {
                    Language.inject(ClientLanguage.loadFrom(resources, List.of("en_us", "zh_cn"), false));
                    requireText("render", "资源包覆盖");
                    requireText("samples", "每帧采样数");
                }
            }
        } finally {
            Language.inject(Language.DEFAULT_INSTANCE);
            for (var path : List.of(override, lang, namespace, assets, packMetadata, metadataRoot, scratch))
                Files.deleteIfExists(path);
        }
        System.out.println("Settings language JAR/Fabric pack/vanilla locale fixture passed");
    }

    private static PackLocationInfo location(String name) {
        return new PackLocationInfo(name, Component.literal(name), PackSource.DEFAULT, Optional.empty());
    }

    private static void requireText(String key, String expected) {
        var actual = Component.translatable("primept.settings." + key).getString();
        require(actual.equals(expected), key + " 的翻译为 " + actual + "，预期 " + expected);
    }

    private static void requireWidgetText(String samples, String quality, String performance) {
        var slider = new VideoOptions.Slider("samples", 1, 64, 1, () -> 32, value -> {},
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.INTEGER);
        var expectedSlider = Options.genericValueLabel(Component.literal(samples), Component.literal("32"));
        require(slider.getMessage().getString().equals(expectedSlider.getString()), "滑块显示一次本地化标题和数值");
        var cycle = VideoOptions.cycle(CycleButton.builder(
                        (String key) -> Component.translatable("primept.settings.quality." + key), "performance")
                .withValues("performance", "quality"), Component.translatable("primept.settings.quality"),
                () -> "performance", value -> {}, error -> { throw new AssertionError(error); });
        var expectedCycle = CommonComponents.optionNameValue(Component.literal(quality), Component.literal(performance));
        require(cycle.getMessage().getString().equals(expectedCycle.getString()), "循环控件显示一次本地化标题和选项值");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    private static void requireStartupNotice(String title, String openGl, String capability) {
        require(Component.translatable("primept.error.unavailable").getString().equals(title), "兼容性通知标题来自游戏语言栈");
        require(new SupportNotice.Problem(true, List.of()).message().getString().startsWith(openGl), "OpenGL 通知本地化");
        String extension = "VK_KHR_ray_tracing_position_fetch";
        String message = new SupportNotice.Problem(false, List.of(extension)).message().getString();
        require(message.startsWith(capability) && message.contains(extension), "扩展通知本地化且保留实际缺失项");
    }

    private static void requireRenderingHierarchy(String enabled, String mode, String offline, String renderer) {
        var toggle = CycleButton.onOffBuilder(true).create(0, 0, 310, 20,
                Component.translatable("primept.settings.enabled"), (button, value) -> {});
        require(toggle.getMessage().getString().equals(CommonComponents.optionNameValue(
                Component.literal(enabled), CommonComponents.OPTION_ON).getString()), "Mod 开关本地化标题只出现一次");
        var modes = CycleButton.builder((RenderMode value) -> Component.translatable("primept.settings.mode." + value.key), RenderMode.OFFLINE)
                .withValues(List.of(RenderMode.values())).create(0, 0, 310, 20,
                        Component.translatable("primept.settings.mode"), (button, value) -> {});
        require(modes.getMessage().getString().equals(CommonComponents.optionNameValue(
                Component.literal(mode), Component.literal(offline)).getString()), "离线模式控件从游戏资源栈读取翻译");
        var renderers = CycleButton.builder((RealtimeRenderer value) -> Component.translatable("primept.settings.realtime_renderer." + value.key), RealtimeRenderer.RESTIR_PT)
                .withValues(List.of(RealtimeRenderer.values())).create(0, 0, 310, 20,
                        Component.translatable("primept.settings.realtime_renderer"), (button, value) -> {});
        require(renderers.getMessage().getString().equals(CommonComponents.optionNameValue(
                Component.literal(renderer), Component.literal("ReSTIR PT Enhanced")).getString()), "实时渲染器控件标题和值只组合一次");
    }

    private static void requireAutoExposure(String caption) {
        var slider = new VideoOptions.Slider("auto_exposure", 0, DisplaySettings.MAX_AUTO_EXPOSURE, 1,
                DisplaySettings.DEFAULT::autoExposure, value -> {},
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.PERCENT);
        var expected = Options.genericValueLabel(Component.literal(caption), Component.literal("65%"));
        require(slider.getMessage().getString().equals(expected.getString()), "自动曝光显示一次本地化标题和百分比");
    }

    private static void requireLighting(String sun, String block) {
        requireText("sun_ev", sun);
        requireText("block_emission_ev", block);
        var slider = new VideoOptions.Slider("sun_ev", -8, 8, 0.25, () -> 1.25, value -> {},
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.LIGHTING_EV);
        var expected = Options.genericValueLabel(Component.literal(sun), Component.literal("+1.25 EV"));
        require(slider.getMessage().getString().equals(expected.getString()), "光照显示本地化标题和有符号 EV");
    }
}
