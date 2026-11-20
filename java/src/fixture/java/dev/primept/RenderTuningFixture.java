package dev.primept;

import java.nio.file.Files;
import java.nio.file.Path;

/** 真实设置所有者验证命令、持久化和诊断优先级；不启动客户端。 */
final class RenderTuningFixture {
    public static void main(String[] arguments) throws Exception {
        var path = Path.of(arguments[0]);
        Files.createDirectories(path.getParent());
        Files.deleteIfExists(path);
        Files.writeString(path, "realtime_renderer=restir_pt\nrestir.cv=true\nrestir.history_cap=20\nrestir.fresh_percent=40\nrr=false\nsamples=1\nexposure_ev=3\n");
        var legacy = new SettingsConfig(path);
        require(!legacy.rr() && legacy.samples() == 2 && legacy.ev() == 3,
                "退役配置迁移保留独立设置，不能整体回退默认");
        legacy.command("pt mode offline");
        require(legacy.samples() == 1, "旧批大小保留在离线设置");
        legacy.setBounces(8);
        var migrated = Files.readString(path);
        require(!migrated.contains("restir.") && !migrated.contains("realtime_renderer") && migrated.contains("offline.samples=1"),
                "保存移除退役键");
        Files.deleteIfExists(path);
        var config = new SettingsConfig(path);
        require(config.tuning().equals(RenderTuning.DEFAULT), "默认关闭诊断");
        config.command("pt diagnostic on");
        config.setQuality(Quality.BALANCED);
        var saved = new SettingsConfig(path);
        require(saved.tuning().equals(new RenderTuning(true)), "命令保存诊断偏好，其他 setter 保留它");
        require(saved.rr() && saved.samples() == 2 && saved.quality() == Quality.BALANCED,
                "诊断保留请求的RR、SPP及画质偏好");
        var before = saved.tuning();
        saved.command("pt mcap 0");
        saved.command("pt fresh 101");
        saved.command("pt cv invalid");
        require(saved.tuning().equals(before), "越界命令不部分发布设置");
        saved.command("pt mode offline");
        require(!saved.tuning().diagnosticActive(saved.selection()) && saved.tuning().diagnostic(),
                "离线优先，诊断偏好保留");
        saved.setOfflineSamples(8);
        var loadedOffline = new SettingsConfig(path);
        loadedOffline.command("pt mode offline");
        require(loadedOffline.samples() == 8, "离线批次仍可保存任意有效SPP");
        saved.command("pt mode realtime");
        require(saved.samples() == 2, "旧任意SPP配置迁移为实时质量策略");
        boolean rejected = false;
        try { saved.setOfflineSamples(1); } catch (IllegalArgumentException expected) { rejected = true; }
        require(rejected && saved.samples() == 2, "旧单样本请求不能覆盖实时质量档");
        require(saved.tuning().diagnosticActive(saved.selection()), "实时恢复诊断有效状态");
        saved.command("pt diagnostic off");
        require(saved.rr() && saved.samples() == 2 && saved.quality() == Quality.BALANCED,
                "退出诊断不需要恢复被覆盖的偏好");
        saved.command("pt spp 1");
        require(saved.samples() == 2 && new SettingsConfig(path).samples() == 2, "重载或旧命令均保持实时 2 spp");
        saved.command("pt mode offline");
        require(saved.samples() == 8, "实时请求不会覆盖离线批次");
        saved.command("pt reset");
        require(new SettingsConfig(path).tuning().equals(RenderTuning.DEFAULT), "恢复默认同时重置新设置");
        System.out.println("配置迁移与原生诊断设置夹具通过");
    }
    private static void require(boolean value, String message) {
        if (!value) throw new AssertionError(message);
    }
}
