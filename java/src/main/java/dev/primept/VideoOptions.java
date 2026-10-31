package dev.primept;

import java.io.IOException;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;
import java.util.function.DoubleSupplier;
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
import net.minecraft.network.chat.Component;

/** 视频列表中的 PT 设置；命令和控件共用 Settings，只有用户操作才写入配置。 */
public final class VideoOptions {
    enum Row { RESET, RENDER, SER, RR, QUALITY, SAMPLES, BOUNCES, DISPLAY, EXPOSURE }

    @FunctionalInterface
    interface Action { void run() throws IOException; }

    @FunctionalInterface
    interface ValueSetter { void set(double value) throws IOException; }

    private final OptionsList list;
    private final Screen screen;
    private final Runnable rebuild;
    private final List<Slider> sliders = new ArrayList<>();
    private CycleButton<Boolean> rr;
    private CycleButton<Boolean> ser;
    private CycleButton<Quality> quality;

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
                list.applyUnsavedChanges();
                if (change(Settings::reset)) rebuild.run();
            }).build());
            case RENDER -> list.addHeader(text("render"));
            case SER -> {
                ser = CycleButton.onOffBuilder(Settings.requestedSer())
                        .withTooltip(value -> Tooltip.create(text("ser.tooltip").copy()
                                .append("\n" + Settings.serStatus())))
                        .create(0, 0, 310, 20, text("ser"), (button, value) -> {
                            change(() -> Settings.setRequestedSer(value));
                            button.setValue(Settings.requestedSer());
                        });
                list.addBig(ser);
            }
            case RR -> {
                rr = CycleButton.onOffBuilder(Settings.requestedRr())
                        .withTooltip(value -> Tooltip.create(text("rr.tooltip").copy()
                                .append("\n" + Settings.rrStatus())))
                        .create(0, 0, 310, 20, text("rr"), (button, value) -> {
                            change(() -> Settings.setRequestedRr(value));
                            button.setValue(Settings.requestedRr());
                        });
                list.addBig(rr);
            }
            case QUALITY -> {
                quality = CycleButton.builder(
                                (Quality value) -> text("quality." + value.key()), Settings.quality())
                        .withValues(List.of(Quality.values()))
                        .withTooltip(OptionInstance.cachedConstantTooltip(text("quality.tooltip")))
                        .create(0, 0, 310, 20, text("quality"), (button, value) -> {
                            change(() -> Settings.setQuality(value));
                            button.setValue(Settings.quality());
                        });
                list.addBig(quality);
            }
            case SAMPLES -> addSlider("samples", Settings.MIN_SAMPLES, Settings.MAX_SAMPLES, 1,
                    Settings::samples, value -> Settings.setSamples((int)value), false);
            case BOUNCES -> addSlider("bounces", Settings.MIN_BOUNCES, Settings.MAX_BOUNCES, 1,
                    Settings::bounces, value -> Settings.setBounces((int)value), false);
            case DISPLAY -> list.addHeader(text("display"));
            case EXPOSURE -> addSlider("exposure", Settings.MIN_EV, Settings.MAX_EV, 0.25,
                    Settings::exposure, value -> Settings.setExposure((float)value), true);
        }
    }

    private void addSlider(String key, double minimum, double maximum, double step,
            DoubleSupplier read, ValueSetter write, boolean ev) {
        var slider = new Slider(key, minimum, maximum, step, read, write, this::failed, ev);
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

    /** 同步命令或其它入口的修改，不触发保存或曝光量化。 */
    public void tick() {
        if (ser.getValue() != Settings.requestedSer()) ser.setValue(Settings.requestedSer());
        if (rr.getValue() != Settings.requestedRr()) rr.setValue(Settings.requestedRr());
        if (quality.getValue() != Settings.quality()) quality.setValue(Settings.quality());
        for (var slider : sliders) slider.sync();
    }

    private static Component text(String key) {
        return Component.translatable("primept.settings." + key);
    }

    /** 拖动按步长保存；初始化和同步保留命令写入的精确值。 */
    static final class Slider extends AbstractSliderButton {
        private final String key;
        private final double minimum, maximum, step;
        private final DoubleSupplier read;
        private final ValueSetter write;
        private final Consumer<Exception> failed;
        private final boolean ev;

        Slider(String key, double minimum, double maximum, double step, DoubleSupplier read,
                ValueSetter write, Consumer<Exception> failed, boolean ev) {
            super(0, 0, 310, 20, Component.empty(), (read.getAsDouble() - minimum) / (maximum - minimum));
            this.key = key;
            this.minimum = minimum;
            this.maximum = maximum;
            this.step = step;
            this.read = read;
            this.write = write;
            this.failed = failed;
            this.ev = ev;
            setTooltip(Tooltip.create(text(key + ".tooltip")));
            updateMessage();
        }

        void sync() {
            value = (read.getAsDouble() - minimum) / (maximum - minimum);
            updateMessage();
        }

        @Override
        protected void updateMessage() {
            String value = ev ? Float.toString((float)read.getAsDouble()) + " EV"
                              : Integer.toString((int)read.getAsDouble());
            setMessage(Options.genericValueLabel(text(key), Component.literal(value)));
        }

        @Override
        protected void applyValue() {
            double next = minimum + Math.round(value * (maximum - minimum) / step) * step;
            try {
                if (next != read.getAsDouble()) write.set(next);
            } catch (IOException | IllegalArgumentException error) {
                failed.accept(error);
            }
            sync();
        }

        @Override
        public boolean keyPressed(KeyEvent event) {
            if (canChangeValue && (event.isLeft() || event.isRight())) {
                double next = read.getAsDouble() + (event.isLeft() ? -step : step);
                setValue((next - minimum) / (maximum - minimum));
                return true;
            }
            return super.keyPressed(event);
        }
    }
}
