package dev.primept;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;
import java.util.function.DoubleSupplier;
import java.util.function.Supplier;
import net.minecraft.client.Minecraft;
import net.minecraft.client.OptionInstance;
import net.minecraft.client.Options;
import net.minecraft.client.gui.components.AbstractSliderButton;
import net.minecraft.client.gui.components.Button;
import net.minecraft.client.gui.components.CycleButton;
import net.minecraft.client.gui.components.OptionsList;
import net.minecraft.client.gui.components.Tooltip;
import net.minecraft.client.gui.screens.AlertScreen;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.input.KeyEvent;
import net.minecraft.client.input.MouseButtonEvent;
import net.minecraft.network.chat.Component;

/** 视频列表中的 PT 设置；命令和控件共用 Settings，只有用户操作才写入配置。 */
public final class VideoOptions {
    enum Row { RESET, RENDER, SER, RR, QUALITY, SAMPLES, BOUNCES, UPDATE_BUDGET, LIGHTING, LATITUDE, SOLAR_LONGITUDE,
        DISPLAY, EXPOSURE, AUTO_EXPOSURE, DRT_SATURATION, DRT_HUE, DRT_COMPRESSION_START }

    @FunctionalInterface
    interface Action { void run() throws IOException; }

    @FunctionalInterface
    interface ValueSetter { void set(double value) throws IOException; }

    @FunctionalInterface
    interface SelectionSetter<T> { void set(T value) throws IOException; }

    enum ValueFormat {
        INTEGER, ANGLE, EXPOSURE, PERCENT;

        String text(double value) {
            return switch (this) {
                case INTEGER -> Integer.toString((int)value);
                case ANGLE -> (int)value + "°";
                case EXPOSURE -> Float.toString((float)value) + " EV";
                case PERCENT -> (int)value + "%";
            };
        }
    }

    private final OptionsList list;
    private final Screen screen;
    private final Runnable rebuild;
    private final List<Slider> sliders = new ArrayList<>();
    private CycleButton<Boolean> rr;
    private CycleButton<Boolean> ser;
    private CycleButton<Quality> quality;
    private CycleButton<UpdateBudget> updateBudget;

    private VideoOptions(OptionsList list, Screen screen, Runnable rebuild) {
        this.list = list;
        this.screen = screen;
        this.rebuild = rebuild;
    }

    /** 在原生选项之前追加；每次屏幕重建创建新 owner。 */
    public static VideoOptions addTo(OptionsList list, Screen screen, Runnable rebuild) {
        var owner = new VideoOptions(list, screen, rebuild);
        for (var row : Row.values()) owner.add(row);
        return owner;
    }

    private void add(Row row) {
        switch (row) {
            case RESET -> list.addBig(Button.builder(text("reset"), button -> {
                if (!applyPendingSliders()) return;
                list.applyUnsavedChanges();
                if (change(Settings::reset)) rebuild.run();
            }).build());
            case RENDER -> list.addHeader(text("render"));
            case SER -> {
                ser = cycle(CycleButton.onOffBuilder(Settings.requestedSer())
                        .withTooltip(value -> Tooltip.create(text("ser.tooltip").copy()
                                .append("\n" + Settings.serStatus()))),
                        text("ser"), Settings::requestedSer, Settings::setRequestedSer, this::failed);
                list.addBig(ser);
            }
            case RR -> {
                rr = cycle(CycleButton.onOffBuilder(Settings.requestedRr())
                        .withTooltip(value -> Tooltip.create(text("rr.tooltip").copy()
                                .append("\n" + Settings.rrStatus()))),
                        text("rr"), Settings::requestedRr, Settings::setRequestedRr, this::failed);
                list.addBig(rr);
            }
            case QUALITY -> {
                quality = cycle(CycleButton.builder(
                                (Quality value) -> text("quality." + value.key()), Settings.quality())
                        .withValues(List.of(Quality.values()))
                        .withTooltip(OptionInstance.cachedConstantTooltip(text("quality.tooltip"))),
                        text("quality"), Settings::quality, Settings::setQuality, this::failed);
                list.addBig(quality);
            }
            case SAMPLES -> addSlider("samples", Settings.MIN_SAMPLES, Settings.MAX_SAMPLES, 1,
                    Settings::samples, value -> Settings.setSamples((int)value), ValueFormat.INTEGER);
            case BOUNCES -> addSlider("bounces", Settings.MIN_BOUNCES, Settings.MAX_BOUNCES, 1,
                    Settings::bounces, value -> Settings.setBounces((int)value), ValueFormat.INTEGER);
            case UPDATE_BUDGET -> {
                updateBudget = cycle(CycleButton.builder(
                                (UpdateBudget value) -> text("update_budget." + value.key), Settings.updateBudget())
                        .withValues(List.of(UpdateBudget.values()))
                        .withTooltip(OptionInstance.cachedConstantTooltip(text("update_budget.tooltip"))),
                        text("update_budget"), Settings::updateBudget, Settings::setUpdateBudget, this::failed);
                list.addBig(updateBudget);
            }
            case LIGHTING -> list.addHeader(text("lighting"));
            case LATITUDE -> addSlider("latitude", -90, 90, 1,
                    () -> Settings.solar().latitude(), value -> Settings.setSolarLatitude((int)value), ValueFormat.ANGLE);
            case SOLAR_LONGITUDE -> addSlider("solar_longitude", 0, 359, 1,
                    () -> Settings.solar().longitude(), value -> Settings.setSolarLongitude((int)value), ValueFormat.ANGLE);
            case DISPLAY -> list.addHeader(text("display"));
            case EXPOSURE -> addSlider("exposure", Settings.MIN_EV, Settings.MAX_EV, 0.25,
                    Settings::exposure, value -> Settings.setExposure((float)value), ValueFormat.EXPOSURE);
            case AUTO_EXPOSURE -> addSlider("auto_exposure", 0, DisplaySettings.MAX_AUTO_EXPOSURE, 1,
                    () -> Settings.display().autoExposure(), value -> Settings.setAutoExposure((int)value), ValueFormat.PERCENT);
            case DRT_SATURATION -> addSlider("drt_saturation", 0, DisplaySettings.MAX_SATURATION, 1,
                    () -> Settings.display().saturation(), value -> Settings.setDrtSaturation((int)value), ValueFormat.PERCENT);
            case DRT_HUE -> addSlider("drt_hue", 0, DisplaySettings.MAX_HUE, 1,
                    () -> Settings.display().hue(), value -> Settings.setDrtHue((int)value), ValueFormat.PERCENT);
            case DRT_COMPRESSION_START -> addSlider("drt_compression_start", 0, DisplaySettings.MAX_COMPRESSION_START, 1,
                    () -> Settings.display().compressionStart(), value -> Settings.setDrtCompressionStart((int)value), ValueFormat.PERCENT);
        }
    }

    // 循环按钮自行组合标题和选项值，builder 的值格式化只返回选项值。
    static <T> CycleButton<T> cycle(CycleButton.Builder<T> builder, Component caption, Supplier<T> read,
            SelectionSetter<T> write, Consumer<Exception> failed) {
        return builder.create(0, 0, 310, 20, caption, (button, value) -> {
            try {
                write.set(value);
            } catch (IOException | IllegalArgumentException error) {
                button.setValue(read.get());
                failed.accept(error);
            }
        });
    }

    private void addSlider(String key, double minimum, double maximum, double step,
            DoubleSupplier read, ValueSetter write, ValueFormat format) {
        var slider = new Slider(key, minimum, maximum, step, read, write, this::failed, format);
        sliders.add(slider);
        list.addBig(slider);
    }

    private boolean change(Action action) {
        try {
            action.run();
            return true;
        } catch (IOException | IllegalArgumentException error) {
            failed(error);
            return false;
        }
    }

    private void failed(Exception error) {
        Minecraft.getInstance().gui.setScreen(new AlertScreen(
                () -> Minecraft.getInstance().gui.setScreen(screen), text("save_failed"),
                Component.literal(error.toString())));
    }

    /** 在宿主确认关闭或恢复默认前提交仍待处理的滑块手势。 */
    public boolean applyPendingSliders() {
        for (var slider : sliders) if (!slider.commitPointerEdit()) return false;
        return true;
    }

    /** 同步命令或其它入口的修改，不触发保存或曝光量化。 */
    public void tick() {
        if (ser.getValue() != Settings.requestedSer()) ser.setValue(Settings.requestedSer());
        if (rr.getValue() != Settings.requestedRr()) rr.setValue(Settings.requestedRr());
        if (quality.getValue() != Settings.quality()) quality.setValue(Settings.quality());
        if (updateBudget.getValue() != Settings.updateBudget()) updateBudget.setValue(Settings.updateBudget());
        for (var slider : sliders) slider.sync();
    }

    private static Component text(String key) {
        return Component.translatable("primept.settings." + key);
    }

    /** 鼠标拖动只预览标签和滑块，松开后提交，键盘单步即时提交。 */
    static final class Slider extends AbstractSliderButton {
        private final Component caption;
        private final double minimum, maximum, step;
        private final DoubleSupplier read;
        private final ValueSetter write;
        private final Consumer<Exception> failed;
        private final ValueFormat format;
        private boolean pointerEditing;
        private double staged = Double.NaN;

        Slider(String key, double minimum, double maximum, double step, DoubleSupplier read,
                ValueSetter write, Consumer<Exception> failed, ValueFormat format) {
            super(0, 0, 310, 20, Component.empty(), (read.getAsDouble() - minimum) / (maximum - minimum));
            this.caption = text(key);
            this.minimum = minimum;
            this.maximum = maximum;
            this.step = step;
            this.read = read;
            this.write = write;
            this.failed = failed;
            this.format = format;
            setTooltip(Tooltip.create(text(key + ".tooltip")));
            updateMessage();
        }

        void sync() {
            if (pointerEditing) return;
            value = toSliderValue(read.getAsDouble());
            updateMessage();
        }

        private double toSliderValue(double value) { return (value - minimum) / (maximum - minimum); }

        @Override
        protected void updateMessage() {
            double shown = Double.isNaN(staged) ? read.getAsDouble() : staged;
            // 滑块直接绘制整条消息，标题只在这里组合一次。
            setMessage(Options.genericValueLabel(caption, Component.literal(format.text(shown))));
        }

        @Override
        protected void applyValue() {
            double next = minimum + Math.round(value * (maximum - minimum) / step) * step;
            if (pointerEditing) {
                staged = next;
                value = toSliderValue(next);
            } else {
                commit(next);
            }
        }

        @Override
        public void onClick(MouseButtonEvent event, boolean doubleClick) {
            pointerEditing = true;
            super.onClick(event, doubleClick);
        }

        @Override
        public void onRelease(MouseButtonEvent event) {
            super.onRelease(event);
            commitPointerEdit();
        }

        boolean commitPointerEdit() {
            if (!pointerEditing) return true;
            pointerEditing = false;
            double next = staged;
            staged = Double.NaN;
            if (Double.isNaN(next)) {
                sync();
                return true;
            }
            return commit(next);
        }

        private boolean commit(double next) {
            try {
                if (next != read.getAsDouble()) write.set(next);
            } catch (IOException | IllegalArgumentException error) {
                sync();
                failed.accept(error);
                return false;
            }
            sync();
            return true;
        }

        @Override
        public boolean keyPressed(KeyEvent event) {
            if (canChangeValue && (event.isLeft() || event.isRight())) {
                double current = Double.isNaN(staged) ? read.getAsDouble() : staged;
                double index = (current - minimum) / step;
                double adjacent = event.isLeft() ? Math.ceil(index) - 1 : Math.floor(index) + 1;
                setValue(toSliderValue(minimum + adjacent * step));
                return true;
            }
            return super.keyPressed(event);
        }
    }
}
