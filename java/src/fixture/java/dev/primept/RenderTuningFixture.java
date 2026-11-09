package dev.primept;

import java.nio.file.Files;
import java.nio.file.Path;

/** 真实设置所有者验证命令、持久化和诊断优先级；不启动客户端。 */
final class RenderTuningFixture {
    public static void main(String[] arguments) throws Exception {
        var path = Path.of(arguments[0]);
        Files.createDirectories(path.getParent());
        Files.deleteIfExists(path);
        Files.writeString(path, "realtime_renderer=restir_pt\nrestir.cv=true\nrestir.history_cap=20\nrestir.fresh_percent=40\nrr=false\nsamples=2\nexposure_ev=3\n");
        var legacy = new SettingsConfig(path);
        require(!legacy.rr() && legacy.samples() == 2 && legacy.ev() == 3,
                "退役配置迁移保留独立设置，不能整体回退默认");
        legacy.setBounces(8);
        var migrated = Files.readString(path);
        require(!migrated.contains("restir.") && !migrated.contains("realtime_renderer"),
                "保存移除退役键");
        Files.deleteIfExists(path);
        var config = new SettingsConfig(path);
        require(config.tuning().equals(RenderTuning.DEFAULT), "默认关闭诊断");
        config.command("pt diagnostic on");
        require(config.command("pt sampling quality").contains("quality"), "质量策略命令使用设置所有者");
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
        saved.setSamples(8);
        require(saved.command("pt sampling performance").contains("仅用于实时") && saved.samples() == 8,
                "实时策略命令不覆盖离线批大小");
        var loadedOffline = new SettingsConfig(path);
        loadedOffline.command("pt mode offline");
        require(loadedOffline.samples() == 8, "离线批次仍可保存任意有效SPP");
        saved.command("pt mode realtime");
        require(saved.samples() == 2, "旧任意SPP配置迁移为实时质量策略");
        boolean rejected = false;
        try { saved.setSamples(3); } catch (IllegalArgumentException expected) { rejected = true; }
        require(rejected && saved.samples() == 2, "实时只发布1/2 spp策略");
        require(saved.tuning().diagnosticActive(saved.selection()), "实时恢复诊断有效状态");
        saved.command("pt diagnostic off");
        require(saved.rr() && saved.samples() == 2 && saved.quality() == Quality.BALANCED,
                "退出诊断不需要恢复被覆盖的偏好");
        saved.command("pt sampling performance");
        require(saved.samples() == 1 && new SettingsConfig(path).samples() == 1,
                "性能策略保存为1 spp，重新读取保持同一策略");
        saved.command("pt sampling invalid");
        require(saved.samples() == 1, "无效策略不改变已发布设置");
        saved.command("pt reset");
        require(new SettingsConfig(path).tuning().equals(RenderTuning.DEFAULT), "恢复默认同时重置新设置");
        System.out.println("配置迁移与原生诊断设置夹具通过");
    }
    private static void require(boolean value, String message) {
        if (!value) throw new AssertionError(message);
    }
}
