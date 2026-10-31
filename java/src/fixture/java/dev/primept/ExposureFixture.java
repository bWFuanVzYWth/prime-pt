package dev.primept;

import java.nio.file.Files;
import java.nio.file.Path;

/** 同源设置的命令、持久化与失败边界；不启动游戏。 */
final class ExposureFixture {
    public static void main(String[] arguments) throws Exception {
        var file = Path.of(arguments[0]);
        Files.deleteIfExists(file);
        defaults(file);
        commandBoundary(file);
        sharedModel(file);
        rejectedInputs(file);
        migration(file);
        serDefaults(file);
        failedSave(file);
        Files.deleteIfExists(file);
        System.out.println("PT settings command/persistence fixture passed");
    }

    private static void defaults(Path file) {
        var config = new SettingsConfig(file);
        require(config.rr() && config.quality() == Quality.PERFORMANCE && config.ev() == 0
                && config.samples() == 1 && config.bounces() == 12 && config.ser(), "默认开启 SER、Performance RR、0 EV、1 SPP、12 顶点");
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
        config.setSamples(3);
        config.setBounces(8);
        config.setRequestedRr(false);
        config.setRequestedSer(false);
        var saved = new SettingsConfig(file);
        require(saved.ev() == -2.5f && saved.quality() == Quality.BALANCED && saved.samples() == 3
                && saved.bounces() == 8 && !saved.rr() && !saved.ser(), "界面 setter 同时持久化全部设置");
        require(saved.command("pt ev").contains("-2.5") && saved.command("pt quality").contains("balanced")
                && saved.command("pt spp").contains("3") && saved.command("pt bounces").contains("8")
                && saved.command("pt rr").contains("off") && saved.command("pt ser").contains("off"),
                "命令查询直接读取同源模型");
        saved.command("pt ev 1");
        saved.command("pt quality quality");
        saved.command("pt spp 5");
        saved.command("pt bounces 16");
        saved.command("pt rr on");
        saved.command("pt ser on");
        var changed = new SettingsConfig(file);
        require(changed.ev() == 1 && changed.quality() == Quality.QUALITY && changed.samples() == 5
                && changed.bounces() == 16 && changed.rr() && changed.ser(), "命令写入与界面 setter 保持一致");
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

    private static void failedSave(Path file) throws Exception {
        var blocked = file.resolveSibling("settings-fixture-blocked");
        Files.writeString(blocked, "blocked");
        try {
            var config = new SettingsConfig(blocked.resolve("config.properties"));
            require(config.command("pt spp 4").contains("保存失败") && config.samples() == 1,
                    "保存失败反馈给用户且保留原内存值");
        } finally { Files.delete(blocked); }
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
