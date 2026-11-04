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

    public static float exposure() { return Holder.CONFIG.ev(); }
    public static boolean requestedRr() { return Holder.CONFIG.rr(); }
    public static boolean requestedSer() { return Holder.CONFIG.ser(); }
    public static Quality quality() { return Holder.CONFIG.quality(); }
    public static int samples() { return Holder.CONFIG.samples(); }
    public static int bounces() { return Holder.CONFIG.bounces(); }
    static UpdateBudget updateBudget() { return Holder.CONFIG.updateBudget(); }
    static SolarSettings solar() { return Holder.CONFIG.solar(); }
    static boolean rr() { return requestedRr() && StreamlineBootstrap.available(); }

    /** 保存成功后才发布新值；输入无效抛出 IllegalArgumentException，IO 失败抛出 IOException。 */
    public static void setExposure(float value) throws IOException { Holder.CONFIG.setExposure(value); }
    public static void setRequestedRr(boolean value) throws IOException { Holder.CONFIG.setRequestedRr(value); }
    public static void setRequestedSer(boolean value) throws IOException { Holder.CONFIG.setRequestedSer(value); }
    public static void setQuality(Quality value) throws IOException { Holder.CONFIG.setQuality(value); }
    public static void setSamples(int value) throws IOException { Holder.CONFIG.setSamples(value); }
    public static void setBounces(int value) throws IOException { Holder.CONFIG.setBounces(value); }
    static void setUpdateBudget(UpdateBudget value) throws IOException { Holder.CONFIG.setUpdateBudget(value); }
    static void setSolarLatitude(int value) throws IOException {
        Holder.CONFIG.setSolar(new SolarSettings(value, solar().longitude()));
    }
    static void setSolarLongitude(int value) throws IOException {
        Holder.CONFIG.setSolar(new SolarSettings(solar().latitude(), value));
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
