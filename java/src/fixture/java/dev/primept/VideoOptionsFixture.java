package dev.primept;

import java.io.IOException;
import java.util.Arrays;
import java.util.List;
import net.minecraft.client.input.KeyEvent;

/** 验证 PT 分栏、步进、精确曝光与保存回退；不创建游戏窗口。 */
final class VideoOptionsFixture {
    public static void main(String[] arguments) {
        layout();
        preciseExposure();
        keyboardStep();
        failedSave();
        System.out.println("Video options layout/precision/rollback fixture passed");
    }

    private static void layout() {
        require(Arrays.asList(VideoOptions.Row.values()).equals(List.of(
                VideoOptions.Row.RESET, VideoOptions.Row.RENDER, VideoOptions.Row.SER, VideoOptions.Row.RR,
                VideoOptions.Row.QUALITY, VideoOptions.Row.SAMPLES, VideoOptions.Row.BOUNCES,
                VideoOptions.Row.DISPLAY, VideoOptions.Row.EXPOSURE)),
                "恢复默认置顶；渲染、显示分栏中的控件恰好各出现一次");
    }

    private static void preciseExposure() {
        double[] current = {1.23456f};
        int[] writes = {0};
        var slider = new VideoOptions.Slider("exposure", -16, 16, 0.25,
                () -> current[0], value -> { current[0] = value; writes[0]++; },
                error -> { throw new AssertionError(error); }, true);
        slider.sync();
        require(writes[0] == 0 && current[0] == (double)1.23456f,
                "创建、同步设置不得把精确命令曝光量化或重新保存");
        require(slider.getMessage().getString().contains("1.23456 EV"), "曝光标签显示当前精确值");
        current[0] = -0.123456f;
        slider.sync();
        require(writes[0] == 0 && slider.getMessage().getString().contains("-0.123456 EV"),
                "外部命令更新立即反映到控件，且不触发写入");
    }

    private static void keyboardStep() {
        double[] current = {32};
        var slider = new VideoOptions.Slider("samples", 1, 64, 1,
                () -> current[0], value -> current[0] = value,
                error -> { throw new AssertionError(error); }, false);
        slider.keyPressed(new KeyEvent(40, 0, 0));
        slider.keyPressed(new KeyEvent(79, 0, 0));
        require(current[0] == 33, "键盘向右增加一个样本，与控件像素宽度无关");
        slider.keyPressed(new KeyEvent(80, 0, 0));
        require(current[0] == 32, "键盘向左减少一个样本");
        current[0] = 0;
        var exposure = new VideoOptions.Slider("exposure", -16, 16, 0.25,
                () -> current[0], value -> current[0] = value,
                error -> { throw new AssertionError(error); }, true);
        exposure.keyPressed(new KeyEvent(40, 0, 0));
        exposure.keyPressed(new KeyEvent(79, 0, 0));
        require(current[0] == 0.25, "曝光键盘步长为 0.25 EV");
    }

    private static void failedSave() {
        int[] failures = {0};
        var slider = new VideoOptions.Slider("bounces", 1, 64, 1,
                () -> 12, value -> { throw new IOException("fixture denied"); },
                error -> failures[0]++, false);
        slider.keyPressed(new KeyEvent(40, 0, 0));
        slider.keyPressed(new KeyEvent(79, 0, 0));
        require(failures[0] == 1 && slider.getMessage().getString().contains("12"),
                "保存失败反馈错误，标签还原为仍生效的值");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
