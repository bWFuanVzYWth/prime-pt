package dev.primept;

import java.io.IOException;
import net.fabricmc.loader.api.FabricLoader;
import net.minecraft.client.Minecraft;
import net.minecraft.network.chat.Component;

/** 客户端本地设置入口；不依赖服务端命令或 Fabric API。 */
public final class Settings {
    public static final float MIN_EV = SettingsConfig.MIN_EV, MAX_EV = SettingsConfig.MAX_EV;
    public static final int MIN_SAMPLES = SettingsConfig.MIN_SAMPLES, MAX_SAMPLES = SettingsConfig.MAX_SAMPLES;
    public static final int MIN_BOUNCES = SettingsConfig.MIN_BOUNCES, MAX_BOUNCES = SettingsConfig.MAX_BOUNCES;
    private static final class Holder {
        private static final SettingsConfig CONFIG = new SettingsConfig(
                FabricLoader.getInstance().getConfigDir().resolve("prime_pt.properties"),
                Boolean.parseBoolean(System.getProperty("primept.ser", "true")));
    }

    private Settings() {}

    public static boolean enabled() { return Holder.CONFIG.selection().enabled(); }
    public static RenderMode mode() { return Holder.CONFIG.selection().mode(); }

    public static boolean offline() { return Holder.CONFIG.selection().offline(); }
    static RenderTuning tuning() { return Holder.CONFIG.tuning(); }
    public static boolean diagnostic() { return tuning().diagnosticActive(Holder.CONFIG.selection()); }

    static void setDiagnostic(boolean value) throws IOException {
        Holder.CONFIG.setTuning(new RenderTuning(value));
    }

    public static void setEnabled(boolean value) throws IOException {
        Holder.CONFIG.setSelection(new RenderSelection(value, mode()));
    }
    public static void setMode(RenderMode value) throws IOException {
        Holder.CONFIG.setSelection(new RenderSelection(enabled(), value));
    }

    public static float exposure() { return Holder.CONFIG.ev(); }
    public static boolean requestedRr() { return Holder.CONFIG.rr(); }
    public static boolean requestedSer() { return Holder.CONFIG.ser(); }
    public static Quality quality() { return Holder.CONFIG.quality(); }
    public static int samples() { return Holder.CONFIG.samples(); }
    public static int bounces() { return Holder.CONFIG.bounces(); }
    static UpdateBudget updateBudget() { return Holder.CONFIG.updateBudget(); }
    static SolarSettings solar() { return Holder.CONFIG.solar(); }
    static DisplaySettings display() { return Holder.CONFIG.display(); }
    static LightingSettings lighting() { return Holder.CONFIG.lighting(); }
    static Denoiser denoiser() { return Holder.CONFIG.denoiser(); }
    static void setDenoiser(Denoiser mode) throws IOException { Holder.CONFIG.setDenoiser(mode); }
    static Denoiser activeDenoiser() {
        if (!enabled() || offline() || diagnostic()) return Denoiser.OFF;
        var choice = denoiser();
        if (choice == Denoiser.DLSS && StreamlineBootstrap.available()) return choice;
        return choice != Denoiser.OFF && Bootstrap.nrdAvailable() ? Denoiser.NRD : Denoiser.OFF;
    }

    /** 保存成功后才发布新值；输入无效抛出 IllegalArgumentException，IO 失败抛出 IOException。 */
    public static void setExposure(float value) throws IOException { Holder.CONFIG.setExposure(value); }
    public static void setRequestedRr(boolean value) throws IOException { Holder.CONFIG.setRequestedRr(value); }
    public static void setRequestedSer(boolean value) throws IOException { Holder.CONFIG.setRequestedSer(value); }
    public static void setQuality(Quality value) throws IOException { Holder.CONFIG.setQuality(value); }
    public static void setBounces(int value) throws IOException { Holder.CONFIG.setBounces(value); }
    static void setUpdateBudget(UpdateBudget value) throws IOException { Holder.CONFIG.setUpdateBudget(value); }
    static void setSolarLatitude(int value) throws IOException {
        Holder.CONFIG.setSolar(new SolarSettings(value, solar().longitude()));
    }
    static void setSolarLongitude(int value) throws IOException {
        Holder.CONFIG.setSolar(new SolarSettings(solar().latitude(), value));
    }
    static void setSunEvQuarters(int value) throws IOException {
        Holder.CONFIG.setLighting(new LightingSettings(value, lighting().blockEmissionEvQuarters()));
    }
    static void setBlockEmissionEvQuarters(int value) throws IOException {
        Holder.CONFIG.setLighting(new LightingSettings(lighting().sunEvQuarters(), value));
    }
    static void setAutoExposure(int value) throws IOException {
        var options = display();
        Holder.CONFIG.setDisplay(new DisplaySettings(value, options.saturation(), options.hue(), options.compressionStart(), options.lensBloom(), options.bloomMode()));
    }
    static void setDrtSaturation(int value) throws IOException {
        var options = display();
        Holder.CONFIG.setDisplay(new DisplaySettings(options.autoExposure(), value, options.hue(), options.compressionStart(), options.lensBloom(), options.bloomMode()));
    }
    static void setDrtHue(int value) throws IOException {
        var options = display();
        Holder.CONFIG.setDisplay(new DisplaySettings(options.autoExposure(), options.saturation(), value, options.compressionStart(), options.lensBloom(), options.bloomMode()));
    }
    static void setDrtCompressionStart(int value) throws IOException {
        var options = display();
        Holder.CONFIG.setDisplay(new DisplaySettings(options.autoExposure(), options.saturation(), options.hue(), value, options.lensBloom(), options.bloomMode()));
    }
    static void setLensBloom(boolean enabled) throws IOException {
        var options = display();
        Holder.CONFIG.setDisplay(new DisplaySettings(options.autoExposure(), options.saturation(), options.hue(), options.compressionStart(), enabled, options.bloomMode()));
    }
    static void setBloomMode(BloomMode bloomMode) throws IOException {
        var options = display();
        Holder.CONFIG.setDisplay(new DisplaySettings(options.autoExposure(), options.saturation(), options.hue(), options.compressionStart(), options.lensBloom(), bloomMode));
    }
    public static void reset() throws IOException { Holder.CONFIG.reset(); }

    public static String rrStatus() { return StreamlineBootstrap.status(); }
    public static String serStatus() { return Bootstrap.serStatus(); }

    /** 返回 true 时调用者必须取消网络发送。 */
    public static boolean command(String input) {
        var message = Holder.CONFIG.command(input);
        if (message == null) return false;
        if (input.trim().matches("pt\\s+rr(?:\\s+.*)?"))
            message += "（" + StreamlineBootstrap.status() + "）";
        if (input.trim().matches("pt\\s+ser(?:\\s+.*)?"))
            message += "（" + Bootstrap.serStatus() + "）";
        Minecraft.getInstance().gui.hud.getChat().addClientSystemMessage(Component.literal(message));
        return true;
    }
}
