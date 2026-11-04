package dev.primept;

import com.mojang.blaze3d.platform.InputConstants;
import java.io.IOException;
import java.util.Arrays;
import java.util.List;
import net.minecraft.client.gui.components.CycleButton;
import net.minecraft.client.input.KeyEvent;
import net.minecraft.client.input.MouseButtonEvent;
import net.minecraft.client.input.MouseButtonInfo;
import net.minecraft.network.chat.CommonComponents;
import net.minecraft.network.chat.Component;

/** 验证 PT 分栏、步进、精确曝光与保存回退；不创建游戏窗口。 */
final class VideoOptionsFixture {
    public static void main(String[] arguments) {
        layout();
        cycleCaptionAndRollback();
        preciseExposure();
        keyboardStep();
        pointerPreview();
        failedSave();
        System.out.println("Video options layout/precision/rollback fixture passed");
    }

    private static void cycleCaptionAndRollback() {
        boolean[] active = {false};
        int[] writes = {0};
        var caption = Component.literal("Test switch");
        String on = CommonComponents.optionNameValue(caption, CommonComponents.OPTION_ON).getString();
        String off = CommonComponents.optionNameValue(caption, CommonComponents.OPTION_OFF).getString();
        var button = VideoOptions.cycle(CycleButton.onOffBuilder(false), caption,
                () -> active[0], value -> {
                    writes[0]++;
                    if (!value) throw new IOException("fixture denied");
                    active[0] = value;
                }, error -> { require(active[0], "保存失败保留原开关值"); });
        require(button.getMessage().getString().equals(off), "宿主开关组合标题和值一次");
        var key = new KeyEvent(40, 0, 0);
        button.onPress(key);
        require(writes[0] == 1 && active[0] && button.getMessage().getString().equals(on),
                "一次按下即时保存新值");
        button.onPress(key);
        require(writes[0] == 2 && active[0] && button.getValue() && button.getMessage().getString().equals(on),
                "保存失败时恢复实际生效的开关标签");
        var choice = VideoOptions.cycle(CycleButton.builder(Component::literal, "Performance")
                .withValues("Performance", "Quality"), Component.literal("DLSS quality"),
                () -> "Performance", value -> {}, error -> { throw new AssertionError(error); });
        require(choice.getMessage().getString().equals(CommonComponents.optionNameValue(
                Component.literal("DLSS quality"), Component.literal("Performance")).getString()), "枚举值只组合一次标题");
    }

    private static void layout() {
        require(Arrays.asList(VideoOptions.Row.values()).equals(List.of(
                VideoOptions.Row.RESET, VideoOptions.Row.RENDER, VideoOptions.Row.SER, VideoOptions.Row.RR,
                VideoOptions.Row.QUALITY, VideoOptions.Row.SAMPLES, VideoOptions.Row.BOUNCES,
                VideoOptions.Row.UPDATE_BUDGET, VideoOptions.Row.LIGHTING, VideoOptions.Row.LATITUDE,
                VideoOptions.Row.SOLAR_LONGITUDE, VideoOptions.Row.DISPLAY, VideoOptions.Row.EXPOSURE,
                VideoOptions.Row.AUTO_EXPOSURE, VideoOptions.Row.DRT_SATURATION, VideoOptions.Row.DRT_HUE,
                VideoOptions.Row.DRT_COMPRESSION_START)),
                "恢复默认置顶，渲染、光照和显示分栏的控件各出现一次");
    }

    private static void preciseExposure() {
        double[] current = {1.23456f};
        int[] writes = {0};
        var slider = new VideoOptions.Slider("exposure", -16, 16, 0.25,
                () -> current[0], value -> { current[0] = value; writes[0]++; },
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.EXPOSURE);
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
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.INTEGER);
        slider.keyPressed(new KeyEvent(40, 0, 0));
        slider.keyPressed(new KeyEvent(79, 0, 0));
        require(current[0] == 33, "键盘向右增加一个样本，与控件像素宽度无关");
        slider.keyPressed(new KeyEvent(80, 0, 0));
        require(current[0] == 32, "键盘向左减少一个样本");
        current[0] = 1.23456f;
        var exposure = new VideoOptions.Slider("exposure", -16, 16, 0.25,
                () -> current[0], value -> current[0] = value,
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.EXPOSURE);
        exposure.keyPressed(new KeyEvent(40, 0, 0));
        exposure.keyPressed(new KeyEvent(79, 0, 0));
        require(current[0] == 1.25, "键盘从精确命令值进入右侧相邻刻度");
        exposure.keyPressed(new KeyEvent(80, 0, 0));
        require(current[0] == 1.0, "曝光键盘步长为 0.25 EV");
    }

    private static void pointerPreview() {
        double[] current = {30};
        int[] writes = {0};
        var slider = new VideoOptions.Slider("latitude", -90, 90, 1,
                () -> current[0], value -> { current[0] = value; writes[0]++; },
                error -> { throw new AssertionError(error); }, VideoOptions.ValueFormat.ANGLE);
        var leftButton = new MouseButtonInfo(InputConstants.MOUSE_BUTTON_LEFT, 0);
        slider.onClick(new MouseButtonEvent(155, 10, leftButton), false);
        require(slider.getMessage().getString().endsWith("0°"), "滑块预览带单位的完整文本");
        slider.mouseDragged(new MouseButtonEvent(306, 10, leftButton), 151, 0);
        slider.sync();
        require(current[0] == 30 && writes[0] == 0 && slider.getMessage().getString().endsWith("90°"),
                "鼠标拖动和同步只更新预览，不写配置");
        require(slider.commitPointerEdit() && slider.commitPointerEdit() && current[0] == 90 && writes[0] == 1,
                "提交最后一个滑块值且只写入一次");
    }

    private static void failedSave() {
        int[] failures = {0};
        var slider = new VideoOptions.Slider("bounces", 1, 64, 1,
                () -> 12, value -> { throw new IOException("fixture denied"); },
                error -> failures[0]++, VideoOptions.ValueFormat.INTEGER);
        slider.keyPressed(new KeyEvent(40, 0, 0));
        slider.keyPressed(new KeyEvent(79, 0, 0));
        require(failures[0] == 1 && slider.getMessage().getString().contains("12"),
                "保存失败反馈错误，标签还原为仍生效的值");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
