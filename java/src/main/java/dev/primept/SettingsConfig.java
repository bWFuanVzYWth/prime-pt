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
                          UpdateBudget updateBudget, SolarSettings solar, DisplaySettings display) {
        private Values {
            if (!Float.isFinite(ev) || ev < MIN_EV || ev > MAX_EV)
                throw new IllegalArgumentException("曝光必须为 [-16, 16] 内的有限 EV");
            if (quality == null) throw new IllegalArgumentException("质量档不能为空");
            if (updateBudget == null) throw new IllegalArgumentException("场景更新档位不能为空");
            range(samples, MIN_SAMPLES, MAX_SAMPLES, "SPP");
            range(bounces, MIN_BOUNCES, MAX_BOUNCES, "路径顶点数");
        }
    }

    private static final Values DEFAULTS = new Values(0, true, Quality.PERFORMANCE, 1, 12, true,
            UpdateBudget.BALANCED, SolarSettings.DEFAULT, DisplaySettings.DEFAULT);
    private final Path file;
    private Values values = DEFAULTS;

    SettingsConfig(Path file) {
        this(file, true);
    }

    SettingsConfig(Path file, boolean defaultSer) {
        this.file = file;
        values = new Values(0, true, Quality.PERFORMANCE, 1, 12, defaultSer,
                UpdateBudget.BALANCED, SolarSettings.DEFAULT, DisplaySettings.DEFAULT);
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
                    readDisplay(properties));
        } catch (IOException | IllegalArgumentException error) {
            System.getLogger("prime_pt").log(System.Logger.Level.WARNING, "PT 配置无效，使用默认值", error);
        }
    }

    float ev() { return values.ev; }
    boolean rr() { return values.rr; }
    boolean ser() { return values.ser; }
    Quality quality() { return values.quality; }
    int samples() { return values.samples; }
    int bounces() { return values.bounces; }
    UpdateBudget updateBudget() { return values.updateBudget; }
    SolarSettings solar() { return values.solar; }
    DisplaySettings display() { return values.display; }

    void setExposure(float ev) throws IOException {
        store(new Values(ev, values.rr, values.quality, values.samples, values.bounces, values.ser, values.updateBudget, values.solar, values.display));
    }

    void setRequestedRr(boolean rr) throws IOException {
        store(new Values(values.ev, rr, values.quality, values.samples, values.bounces, values.ser, values.updateBudget, values.solar, values.display));
    }

    void setRequestedSer(boolean ser) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, ser, values.updateBudget, values.solar, values.display));
    }

    void setQuality(Quality quality) throws IOException {
        store(new Values(values.ev, values.rr, quality, values.samples, values.bounces, values.ser, values.updateBudget, values.solar, values.display));
    }

    void setSamples(int samples) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, samples, values.bounces, values.ser, values.updateBudget, values.solar, values.display));
    }

    void setBounces(int bounces) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, bounces, values.ser, values.updateBudget, values.solar, values.display));
    }

    void setUpdateBudget(UpdateBudget budget) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser, budget, values.solar, values.display));
    }

    void setSolar(SolarSettings solar) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser, values.updateBudget, solar, values.display));
    }

    void setDisplay(DisplaySettings display) throws IOException {
        store(new Values(values.ev, values.rr, values.quality, values.samples, values.bounces, values.ser, values.updateBudget, values.solar, display));
    }

    void reset() throws IOException { store(DEFAULTS); }

    private static DisplaySettings readDisplay(Properties properties) {
        var defaults = DisplaySettings.DEFAULT;
        return new DisplaySettings(
                readPercent(properties, "display.auto_exposure_percent", defaults.autoExposure()),
                readPercent(properties, "display.saturation_percent", defaults.saturation()),
                readPercent(properties, "display.hue_percent", defaults.hue()),
                readPercent(properties, "display.compression_start_percent", defaults.compressionStart()));
    }

    private static int readPercent(Properties properties, String key, int fallback) {
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
                    return "PT SPP：" + samples() + "（范围 [1, 64]）";
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
        return "SER " + (ser() ? "on" : "off") + " / RR " + (rr() ? "on" : "off") + " / " + quality().key() + " / " + ev()
                + " EV / " + samples() + " SPP / " + bounces() + " 顶点";
    }

    private void store(Values next) throws IOException {
        var target = file.toAbsolutePath();
        Files.createDirectories(target.getParent());
        var temporary = Files.createTempFile(target.getParent(), "prime-pt-", ".properties");
        try {
            Files.writeString(temporary, "exposure_ev=" + next.ev + "\nrr=" + next.rr
                    + "\nquality=" + next.quality.key() + "\nsamples=" + next.samples + "\nbounces=" + next.bounces
                    + "\nser=" + next.ser + "\nupdate_budget=" + next.updateBudget.key
                    + "\nastronomy.latitude_degrees=" + next.solar.latitude()
                    + "\nastronomy.solar_longitude_degrees=" + next.solar.longitude()
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
        return "用法：/pt ser [on|off]；/pt rr [on|off]；/pt quality [dlaa|quality|balanced|performance|ultra_performance]；"
                + "/pt ev [EV]；/pt spp [1..64]；/pt bounces [1..64]；/pt reset";
    }
}
