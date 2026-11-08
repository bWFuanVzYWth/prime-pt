package dev.primept;

import java.nio.file.Files;
import java.nio.file.Path;

/** 真实设置所有者验证命令、持久化和诊断优先级；不启动客户端。 */
final class RenderTuningFixture {
    public static void main(String[] arguments) throws Exception {
        var path = Path.of(arguments[0]);
        Files.createDirectories(path.getParent());
        Files.deleteIfExists(path);
        var config = new SettingsConfig(path);
        require(config.tuning().equals(RenderTuning.DEFAULT), "默认M cap=20、fresh=40%、关闭诊断");
        config.command("pt mcap 100");
        require(config.tuning().restirCv(), "默认开启ReSTCV");
        config.command("pt cv off");
        config.command("pt fresh 100");
        config.command("pt diagnostic on");
        config.setSamples(2);
        config.setQuality(Quality.BALANCED);
        var saved = new SettingsConfig(path);
        require(saved.tuning().equals(new RenderTuning(100, 100, true, false)), "命令保存CV开关，其他setter保留它们");
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
        var loadedOffline = new SettingsConfig(path);
        loadedOffline.command("pt mode offline");
        require(loadedOffline.samples() == 8, "离线批次仍可保存任意有效SPP");
        saved.command("pt mode realtime");
        require(saved.samples() == 2, "旧任意SPP配置迁移为实时2 spp分离");
        boolean rejected = false;
        try { saved.setSamples(3); } catch (IllegalArgumentException expected) { rejected = true; }
        require(rejected && saved.samples() == 2, "实时只发布1/2 spp策略");
        require(saved.tuning().diagnosticActive(saved.selection()), "实时恢复诊断有效状态");
        saved.command("pt diagnostic off");
        require(saved.rr() && saved.samples() == 2 && saved.quality() == Quality.BALANCED,
                "退出诊断不需要恢复被覆盖的偏好");
        saved.command("pt reset");
        require(new SettingsConfig(path).tuning().equals(RenderTuning.DEFAULT), "恢复默认同时重置新设置");
        System.out.println("ReSTIR采样和原生诊断设置夹具通过");
    }
    private static void require(boolean value, String message) {
        if (!value) throw new AssertionError(message);
    }
}
