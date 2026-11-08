package dev.primept;

import java.io.IOException;
import java.nio.file.AccessDeniedException;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Properties;

/** 主线程拥有的设置；命令与视频界面共用校验和保存，保存失败保留原值。 */
final class SettingsConfig {
    static final float MIN_EV = -16, MAX_EV = 16;
    static final int MIN_SAMPLES = 1, MAX_SAMPLES = 64;
    static final int MIN_BOUNCES = 1, MAX_BOUNCES = 64;

    private record Values(float ev, boolean rr, Quality quality, int samples, int bounces, boolean ser,
                          UpdateBudget updateBudget, SolarSettings solar, DisplaySettings display,
                          LightingSettings lighting, RenderSelection selection, RenderTuning tuning) {
        private Values {
            if (!Float.isFinite(ev) || ev < MIN_EV || ev > MAX_EV)
                throw new IllegalArgumentException("曝光必须为 [-16, 16] 内的有限 EV");
            if (selection == null) throw new IllegalArgumentException("渲染选择不能为空");
            if (tuning == null) throw new IllegalArgumentException("输出采样设置不能为空");
            if (quality == null) throw new IllegalArgumentException("质量档不能为空");
            if (updateBudget == null) throw new IllegalArgumentException("场景更新档位不能为空");
            range(samples, MIN_SAMPLES, MAX_SAMPLES, "SPP");
            range(bounces, MIN_BOUNCES, MAX_BOUNCES, "路径顶点数");
        }
    }

    private static final Values DEFAULTS = new Values(0, true, Quality.PERFORMANCE, 1, 12, true,
            UpdateBudget.BALANCED, SolarSettings.DEFAULT, DisplaySettings.DEFAULT, LightingSettings.DEFAULT, RenderSelection.DEFAULT, RenderTuning.DEFAULT);
    private final Path file;
    private Values values = DEFAULTS;

    SettingsConfig(Path file) {
        this(file, true);
    }

    SettingsConfig(Path file, boolean defaultSer) {
        this.file = file;
        values = new Values(0, true, Quality.PERFORMANCE, 1, 12, defaultSer,
                UpdateBudget.BALANCED, SolarSettings.DEFAULT, DisplaySettings.DEFAULT, LightingSettings.DEFAULT, RenderSelection.DEFAULT, RenderTuning.DEFAULT);
        Path source = file;
        if (!Files.exists(source) && source.getFileName().toString().equals("prime_pt.properties"))
            source = file.resolveSibling("primary_rt.properties");
        if (!Files.exists(source)) return;
        var properties = new Properties();
        try (var input = Files.newBufferedReader(source)) {
            properties.load(input);
            values = new Values(parseEv(properties.getProperty("exposure_ev", "0")),
                    parseBoolean(properties, "rr", true), Quality.parse(properties.getProperty("quality", "performance")),
                    Integer.parseInt(properties.getProperty("samples", "1")),
                    Integer.parseInt(properties.getProperty("bounces", "12")), parseBoolean(properties, "ser", defaultSer),
                    UpdateBudget.parse(properties.getProperty("update_budget", "balanced")),
                    new SolarSettings(Integer.parseInt(properties.getProperty("astronomy.latitude_degrees", "30")),
                            Integer.parseInt(properties.getProperty("astronomy.solar_longitude_degrees", "0"))),
                    readDisplay(properties), readLighting(properties), new RenderSelection(
                            parseBoolean(properties, "enabled", true), RenderMode.REALTIME,
                            RealtimeRenderer.parse(properties.getProperty("realtime_renderer", "path_trace"))),
                    new RenderTuning(readInt(properties, "restir.history_cap", 20),
                        readInt(properties, "restir.fresh_percent", 40), parseBoolean(properties, "diagnostic.raw", false),
                        parseBoolean(properties, "restir.cv", true)));
        } catch (IOException | IllegalArgumentException error) {
            System.getLogger("prime_pt").log(System.Logger.Level.WARNING, "PT 配置无效，使用默认值", error);
        }
    }

    float ev() { return values.ev; }
    boolean rr() { return values.rr; }
    boolean ser() { return values.ser; }
    Quality quality() { return values.quality; }
    int samples() { return values.selection.mode() == RenderMode.OFFLINE ? values.samples : Math.min(2, values.samples); }
    int bounces() { return values.bounces; }
    UpdateBudget updateBudget() { return values.updateBudget; }
    SolarSettings solar() { return values.solar; }
    DisplaySettings display() { return values.display; }
    LightingSettings lighting() { return values.lighting; }

    void setExposure(float ev) throws IOException {
        store(new Values(ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setRequestedRr(boolean rr) throws IOException {
        store(new Values(values.ev, rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setRequestedSer(boolean ser) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setQuality(Quality quality) throws IOException {
        store(new Values(values.ev, values.rr, quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setSamples(int samples) throws IOException {
        if (values.selection.mode() != RenderMode.OFFLINE) range(samples, 1, 2, "实时采样策略");
        store(new Values(values.ev, values.rr, values.quality, samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setBounces(int bounces) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setUpdateBudget(UpdateBudget budget) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                budget, values.solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setSolar(SolarSettings solar) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, solar, values.display, values.lighting, values.selection, values.tuning));
    }

    void setDisplay(DisplaySettings display) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, display, values.lighting, values.selection, values.tuning));
    }

    void setLighting(LightingSettings lighting) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, lighting, values.selection, values.tuning));
    }

    RenderSelection selection() { return values.selection; }
    RenderTuning tuning() { return values.tuning; }

    void setTuning(RenderTuning tuning) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, values.selection, tuning));
    }

    void setSelection(RenderSelection selection) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser,
                values.updateBudget, values.solar, values.display, values.lighting, selection, values.tuning));
    }

    void reset() throws IOException { store(DEFAULTS); }

    private static DisplaySettings readDisplay(Properties properties) {
        var defaults = DisplaySettings.DEFAULT;
        return new DisplaySettings(
                readInt(properties, "display.auto_exposure_percent", defaults.autoExposure()),
                readInt(properties, "display.saturation_percent", defaults.saturation()),
                readInt(properties, "display.hue_percent", defaults.hue()),
                readInt(properties, "display.compression_start_percent", defaults.compressionStart()));
    }

    private static LightingSettings readLighting(Properties properties) {
        var defaults = LightingSettings.DEFAULT;
        return new LightingSettings(
                readInt(properties, "lighting.sun_ev_quarters", defaults.sunEvQuarters()),
                readInt(properties, "lighting.block_emission_ev_quarters", defaults.blockEmissionEvQuarters()));
    }

    private static int readInt(Properties properties, String key, int fallback) {
        return Integer.parseInt(properties.getProperty(key, Integer.toString(fallback)));
    }

    private static boolean parseBoolean(Properties properties, String key, boolean fallback) {
        String value = properties.getProperty(key, Boolean.toString(fallback));
        if (!value.equals("true") && !value.equals("false"))
            throw new IllegalArgumentException(key + " 必须为 true 或 false");
        return Boolean.parseBoolean(value);
    }

    private static float parseEv(String value) {
        float ev;
        try { ev = Float.parseFloat(value); }
        catch (NumberFormatException error) {
            throw new IllegalArgumentException("曝光必须为 [-16, 16] 内的有限 EV");
        }
        if (!Float.isFinite(ev) || ev < MIN_EV || ev > MAX_EV)
            throw new IllegalArgumentException("曝光必须为 [-16, 16] 内的有限 EV");
        return ev;
    }

    private static void range(int value, int min, int max, String label) {
        if (value < min || value > max)
            throw new IllegalArgumentException(label + " 必须为 [" + min + ", " + max + "] 内的整数");
    }

    /** 输入不含开头斜杠；返回 null 表示继续发送原命令。 */
    String command(String input) {
        String[] parts = input.trim().split("\\s+");
        if (!parts[0].equals("pt")) return null;
        if (parts.length == 1) return status() + "；" + usage();
        if (parts.length > 3) return usage();
        try {
            switch (parts[1]) {
                case "cv" -> {
                    if (parts.length == 3) {
                        if (!parts[2].equals("on") && !parts[2].equals("off"))
                            throw new IllegalArgumentException("ReSTCV开关必须为on或off");
                        setTuning(new RenderTuning(tuning().historyCap(), tuning().freshPercent(), tuning().diagnostic(), parts[2].equals("on")));
                    }
                    return "ReSTCV：" + (tuning().restirCv() ? "on" : "off");
                }
                case "diagnostic" -> {
                    if (parts.length == 3) {
                        if (!parts[2].equals("on") && !parts[2].equals("off")) throw new IllegalArgumentException("诊断开关必须为on或off");
                        setTuning(new RenderTuning(tuning().historyCap(), tuning().freshPercent(), parts[2].equals("on"), tuning().restirCv()));
                    }
                    return "原生无累积诊断：" + (tuning().diagnostic() ? "on" : "off");
                }
                case "mcap" -> {
                    if (parts.length == 3) setTuning(new RenderTuning(Integer.parseInt(parts[2]), tuning().freshPercent(), tuning().diagnostic(), tuning().restirCv()));
                    return "ReSTIR历史M上限：" + tuning().historyCap();
                }
                case "fresh" -> {
                    if (parts.length == 3) setTuning(new RenderTuning(tuning().historyCap(), Integer.parseInt(parts[2]), tuning().diagnostic(), tuning().restirCv()));
                    return "ReSTIR初采输出比例：" + tuning().freshPercent() + "%";
                }
                case "enabled" -> {
                    if (parts.length == 3) {
                        if (!parts[2].equals("on") && !parts[2].equals("off"))
                            throw new IllegalArgumentException("Mod 开关必须为 on 或 off");
                        setSelection(new RenderSelection(parts[2].equals("on"), selection().mode(), selection().renderer()));
                    }
                    return "Prime PT：" + (selection().enabled() ? "on" : "off");
                }
                case "mode" -> {
                    if (parts.length == 3) setSelection(new RenderSelection(selection().enabled(), RenderMode.parse(parts[2]), selection().renderer()));
                    return "Prime PT 模式：" + selection().mode().key;
                }
                case "renderer" -> {
                    if (parts.length == 3) setSelection(new RenderSelection(selection().enabled(), selection().mode(), RealtimeRenderer.parse(parts[2])));
                    return "Prime PT 实时渲染器：" + selection().renderer().key;
                }
                case "ser" -> {
                    if (parts.length == 3) {
                        if (!parts[2].equals("on") && !parts[2].equals("off"))
                            throw new IllegalArgumentException("SER 必须为 on 或 off");
                        setRequestedSer(parts[2].equals("on"));
                    }
                    return "SER 请求：" + (ser() ? "on" : "off");
                }
                case "rr" -> {
                    if (parts.length == 3) {
                        if (!parts[2].equals("on") && !parts[2].equals("off"))
                            throw new IllegalArgumentException("RR 必须为 on 或 off");
                        setRequestedRr(parts[2].equals("on"));
                    }
                    return "DLSS RR 请求：" + (rr() ? "on" : "off");
                }
                case "quality" -> {
                    if (parts.length == 3) setQuality(Quality.parse(parts[2]));
                    return "PT 质量：" + quality().key() + "（" + quality().label() + "）";
                }
                case "ev", "exposure" -> {
                    if (parts.length == 3) setExposure(parseEv(parts[2]));
                    return "PT 曝光：" + ev() + " EV（+1 EV 亮度加倍）";
                }
                case "spp" -> {
                    if (parts.length == 3) setSamples(parseInteger(parts[2], "SPP"));
                    return "PT SPP：" + samples() + (values.selection.mode() == RenderMode.OFFLINE ? "（离线批大小 [1, 64]）" : "（1 随机 / 2 首表面分离）");
                }
                case "bounces" -> {
                    if (parts.length == 3) setBounces(parseInteger(parts[2], "路径顶点数"));
                    return "PT 路径顶点数：" + bounces() + "（范围 [1, 64]）";
                }
                case "reset" -> {
                    if (parts.length != 2) return usage();
                    reset();
                    return "PT 已恢复默认值：" + status();
                }
                default -> { return usage(); }
            }
        } catch (IllegalArgumentException error) {
            return error.getMessage();
        } catch (IOException error) {
            return "PT 设置保存失败：" + error.getMessage();
        }
    }

    private static int parseInteger(String input, String label) {
        try { return Integer.parseInt(input); }
        catch (NumberFormatException error) {
            throw new IllegalArgumentException(label + " 必须为 [1, 64] 内的整数");
        }
    }

    private String status() {
        return "Prime " + selection().enabled() + " / " + selection().mode().key + " / " + selection().renderer().key + " / SER " + (ser() ? "on" : "off") + " / RR " + (rr() ? "on" : "off") + " / " + quality().key() + " / " + ev()
                + " EV / " + samples() + " SPP / " + bounces() + " 顶点 / M cap " + tuning().historyCap()
                + " / fresh " + tuning().freshPercent() + "% / ReSTCV " + (tuning().restirCv() ? "on" : "off")
                + " / 原生诊断 " + (tuning().diagnostic() ? "on" : "off");
    }

    private void store(Values next) throws IOException {
        var target = file.toAbsolutePath();
        Files.createDirectories(target.getParent());
        var temporary = Files.createTempFile(target.getParent(), "prime-pt-", ".properties");
        try {
            Files.writeString(temporary, "enabled=" + next.selection.enabled()
                    + "\nrestir.history_cap=" + next.tuning.historyCap() + "\nrestir.fresh_percent=" + next.tuning.freshPercent()
                    + "\ndiagnostic.raw=" + next.tuning.diagnostic()
                    + "\nrestir.cv=" + next.tuning.restirCv()
                    + "\nrealtime_renderer=" + next.selection.renderer().key + "\nexposure_ev=" + next.ev + "\nrr=" + next.rr
                    + "\nquality=" + next.quality.key() + "\nsamples=" + next.samples + "\nbounces=" + next.bounces
                    + "\nser=" + next.ser + "\nupdate_budget=" + next.updateBudget.key
                    + "\nastronomy.latitude_degrees=" + next.solar.latitude()
                    + "\nastronomy.solar_longitude_degrees=" + next.solar.longitude()
                    + "\nlighting.sun_ev_quarters=" + next.lighting.sunEvQuarters()
                    + "\nlighting.block_emission_ev_quarters=" + next.lighting.blockEmissionEvQuarters()
                    + "\ndisplay.auto_exposure_percent=" + next.display.autoExposure()
                    + "\ndisplay.saturation_percent=" + next.display.saturation()
                    + "\ndisplay.hue_percent=" + next.display.hue()
                    + "\ndisplay.compression_start_percent=" + next.display.compressionStart() + "\n");
            replace(temporary, target);
            values = next;
        } finally { Files.deleteIfExists(temporary); }
    }

    private static void replace(Path source, Path target) throws IOException {
        for (int attempt = 0; ; attempt++) {
            try {
                try {
                    Files.move(source, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
                } catch (AtomicMoveNotSupportedException error) {
                    Files.move(source, target, StandardCopyOption.REPLACE_EXISTING);
                }
                return;
            } catch (AccessDeniedException error) {
                // Windows 文件扫描可能暂时阻止替换；仅对可写普通目标重试，最多阻塞 70ms。
                if (attempt == 3 || !Files.isRegularFile(target) || !Files.isWritable(target)) throw error;
                try { Thread.sleep(10L << attempt); }
                catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                    throw new IOException("配置替换被中断", interrupted);
                }
            }
        }
    }

    private static String usage() {
        return "用法：/pt enabled [on|off]；/pt mode [realtime|offline]；/pt renderer [path_trace|restir_pt]；/pt ser [on|off]；/pt rr [on|off]；/pt quality [dlaa|quality|balanced|performance|ultra_performance]；"
                + "/pt ev [EV]；/pt spp [1|2，离线1..64]；/pt bounces [1..64]；/pt mcap [1..100]；/pt fresh [0..100]；/pt cv [on|off]；/pt diagnostic [on|off]；/pt reset";
    }
}
