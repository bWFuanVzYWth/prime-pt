package dev.primept;

/** 稳定配置键与 native 帧协议值，兼容旧 rr 布尔设置。 */
enum Denoiser {
    DLSS("dlss", 1), NRD("nrd", 2), OFF("off", 0);
    final String key;
    final int wire;
    Denoiser(String key, int wire) {
        this.key = key;
        this.wire = wire;
    }
    static Denoiser parse(String text) {
        for (var mode : values()) if (mode.key.equalsIgnoreCase(text)) return mode;
        throw new IllegalArgumentException("降噪器可选 dlss、nrd 或 off");
    }
}
