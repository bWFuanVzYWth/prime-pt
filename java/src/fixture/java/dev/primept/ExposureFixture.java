package dev.primept;

import java.nio.file.Files;
import java.nio.file.Path;

/** 同源设置的命令、持久化与失败边界；不启动游戏。 */
final class ExposureFixture {
    public static void main(String[] arguments) throws Exception {
        var file = Path.of(arguments[0]);
        Files.deleteIfExists(file);
        defaults(file);
        renderHierarchy(file);
        commandBoundary(file);
        sharedModel(file);
        rejectedInputs(file);
        migration(file);
        solarSettings(file);
        displaySettings(file);
        lightingSettings(file);
        serDefaults(file);
        failedSave(file);
        Files.deleteIfExists(file);
        System.out.println("PT settings command/persistence fixture passed");
    }

    private static void renderHierarchy(Path file) throws Exception {
        var config = new SettingsConfig(file);
        config.command("pt mode offline");
        config.command("pt enabled off");
        require(!config.selection().enabled() && !config.selection().offline()
                && config.selection().mode() == RenderMode.OFFLINE,
                "关闭 Mod 保留下层偏好，同时停止离线有效状态");
        config.command("pt enabled on");
        require(config.selection().offline(), "本次会话重新启用保留离线选择");
        var loaded = new SettingsConfig(file);
        require(loaded.selection().enabled() && loaded.selection().mode() == RenderMode.REALTIME,
                "重新加载只恢复 Mod 开关和实时选择，离线快照不跨会话恢复");
        config.reset();
    }

    private static void defaults(Path file) {
        var config = new SettingsConfig(file);
        require(config.rr() && config.quality() == Quality.PERFORMANCE && config.ev() == 0
                && config.samples() == 1 && config.bounces() == 12 && config.ser()
                && config.updateBudget() == UpdateBudget.BALANCED, "默认开启 SER、Performance RR、0 EV、1 SPP、12 顶点和平衡更新");
        require(config.display().equals(new DisplaySettings(65, 5, 75, 40)), "自动曝光和 DRT 各自使用显示默认值");
        require(config.lighting().equals(LightingSettings.DEFAULT), "太阳和方块发光默认各为 0 EV");
    }

    private static void commandBoundary(Path file) {
        var config = new SettingsConfig(file);
        require(config.command("pt ev") != null, "接管 /pt");
        require(config.command("ptx ev 4") == null && config.command("primept ev 4") == null,
                "不截获其它网络命令和旧前缀");
        require(config.command("pt").contains("performance"), "根命令查询当前设置");
    }

    private static void sharedModel(Path file) throws Exception {
        var config = new SettingsConfig(file);
        config.setExposure(-2.5f);
        config.setQuality(Quality.BALANCED);
        config.setSamples(2);
        config.setBounces(8);
        config.setRequestedRr(false);
        config.setRequestedSer(false);
        config.setUpdateBudget(UpdateBudget.SMOOTH);
        var saved = new SettingsConfig(file);
        require(saved.ev() == -2.5f && saved.quality() == Quality.BALANCED && saved.samples() == 2
                && saved.bounces() == 8 && !saved.rr() && !saved.ser()
                && saved.updateBudget() == UpdateBudget.SMOOTH, "界面 setter 同时持久化全部设置");
        require(saved.command("pt ev").contains("-2.5") && saved.command("pt quality").contains("balanced")
                && saved.command("pt spp").contains("2") && saved.command("pt bounces").contains("8")
                && saved.command("pt rr").contains("off") && saved.command("pt ser").contains("off"),
                "命令查询直接读取同源模型");
        saved.command("pt ev 1");
        saved.command("pt quality quality");
        saved.command("pt spp 1");
        saved.command("pt bounces 16");
        saved.command("pt rr on");
        saved.command("pt ser on");
        var changed = new SettingsConfig(file);
        require(changed.ev() == 1 && changed.quality() == Quality.QUALITY && changed.samples() == 1
                && changed.bounces() == 16 && changed.rr() && changed.ser()
                && changed.updateBudget() == UpdateBudget.SMOOTH, "命令写入与界面 setter 保持一致");
        changed.command("pt reset");
        defaults(file);
    }

    private static void rejectedInputs(Path file) throws Exception {
        var config = new SettingsConfig(file);
        config.setExposure(-2.5f);
        for (var invalid : new String[] {"NaN", "Infinity", "-17", "17", "text"}) {
            config.command("pt ev " + invalid);
            require(config.ev() == -2.5f && new SettingsConfig(file).ev() == -2.5f, "无效曝光不改变内存或配置");
        }
        config.command("pt spp 0");
        config.command("pt spp 65");
        config.command("pt bounces 0");
        config.command("pt bounces 65");
        config.command("pt bounces 1.5");
        config.command("pt quality invalid");
        config.command("pt rr invalid");
        config.command("pt ser invalid");
        require(config.samples() == 1 && config.bounces() == 12 && config.quality() == Quality.PERFORMANCE
                && config.rr() && config.ser(), "无效值保持原模型");
        config.command("pt ev -16");
        config.command("pt mode offline");
        config.command("pt spp 64");
        config.command("pt bounces 1");
        require(config.ev() == -16 && config.samples() == 64 && config.bounces() == 1, "允许范围端点");
    }

    private static void migration(Path file) throws Exception {
        Files.writeString(file, "exposure_ev=-3\nrr=false\n");
        var legacy = new SettingsConfig(file);
        require(legacy.ev() == -3 && !legacy.rr() && legacy.quality() == Quality.PERFORMANCE && legacy.ser(),
                "旧配置保留曝光/RR，同时补入 Performance 与自动 SER");
        legacy.setSamples(2);
        var migrated = new SettingsConfig(file);
        require(migrated.ev() == -3 && !migrated.rr() && migrated.quality() == Quality.PERFORMANCE
                && migrated.samples() == 2, "迁移后的保存保留全部旧值");
    }

    private static void serDefaults(Path file) throws Exception {
        Files.writeString(file, "exposure_ev=-1\n");
        require(!new SettingsConfig(file, false).ser(), "启动参数仅为缺失的 SER 配置提供默认值");
        var config = new SettingsConfig(file);
        config.command("pt ser off");
        require(!new SettingsConfig(file, true).ser(), "已保存关闭值优先于启动默认值");
        config.command("pt ser on");
        require(new SettingsConfig(file, false).ser(), "已保存开启值优先于启动默认值");
        config.command("pt reset");
        require(new SettingsConfig(file, false).ser(), "恢复默认写回自动开启，不沿用旧启动默认值");
    }

    private static void solarSettings(Path file) throws Exception {
        var config = new SettingsConfig(file);
        require(config.solar().equals(SolarSettings.DEFAULT), "旧配置使用默认纬度和黄经");
        var solstice = new SolarSettings(-45, 270);
        config.setSolar(solstice);
        config.setExposure(1);
        var saved = new SettingsConfig(file);
        require(saved.solar().equals(solstice), "其它设置保存时保留太阳轨迹参数");
        saved.reset();
        require(new SettingsConfig(file).solar().equals(SolarSettings.DEFAULT), "恢复太阳轨迹默认值");
    }

    private static void displaySettings(Path file) throws Exception {
        Files.writeString(file, "exposure_ev=-3\ndisplay.saturation_percent=12\n");
        var config = new SettingsConfig(file);
        require(config.display().equals(new DisplaySettings(65, 12, 75, 40)), "读取旧版显示键并补入缺省值");
        var next = new DisplaySettings(0, 5, 100, 99);
        config.setDisplay(next);
        require(config.ev() == -3, "修改自动曝光和 DRT 保留手动 EV");
        config.command("pt ev 1.23456");
        var saved = new SettingsConfig(file);
        require(saved.ev() == 1.23456f && saved.display().equals(next), "修改手动 EV 保留自动曝光和 DRT 配置");
        saved.reset();
        defaults(file);
    }

    private static void lightingSettings(Path file) throws Exception {
        Files.writeString(file, "exposure_ev=-3\nlighting.sun_ev_quarters=-7\nlighting.block_emission_ev_quarters=17\n");
        var config = new SettingsConfig(file);
        require(config.lighting().sunEv() == -1.75f && config.lighting().blockEmissionEv() == 4.25f,
                "沿用旧配置的四分之一 EV 单位");
        var next = new LightingSettings(-32, 40);
        config.setLighting(next);
        require(config.ev() == -3, "调节光源强度保留手动曝光");
        config.setExposure(2f);
        var saved = new SettingsConfig(file);
        require(saved.lighting().equals(next) && saved.ev() == 2f, "光照与曝光配置独立保存");
        saved.reset();
        require(new SettingsConfig(file).lighting().equals(LightingSettings.DEFAULT), "恢复旧版光照默认值");
    }

    private static void failedSave(Path file) throws Exception {
        var blocked = file.resolveSibling("settings-fixture-blocked");
        Files.writeString(blocked, "blocked");
        try {
            var config = new SettingsConfig(blocked.resolve("config.properties"));
            require(config.command("pt spp 2").contains("保存失败") && config.samples() == 1,
                    "保存失败反馈给用户且保留原内存值");
        } finally { Files.delete(blocked); }
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
