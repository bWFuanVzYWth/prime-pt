package dev.primept;

/** 呈现参数的配置值，均以整数百分比保存。手动 EV 由独立设置管理。 */
record DisplaySettings(int autoExposure, int saturation, int hue, int compressionStart) {
    static final int MAX_AUTO_EXPOSURE = 100;
    static final int MAX_SATURATION = 50;
    static final int MAX_HUE = 100;
    static final int MAX_COMPRESSION_START = 99;
    static final DisplaySettings DEFAULT = new DisplaySettings(65, 5, 75, 40);

    DisplaySettings {
        requirePercent(autoExposure, MAX_AUTO_EXPOSURE, "自动曝光强度");
        requirePercent(saturation, MAX_SATURATION, "饱和度补偿");
        requirePercent(hue, MAX_HUE, "色相补偿");
        requirePercent(compressionStart, MAX_COMPRESSION_START, "压缩起点");
    }

    private static void requirePercent(int value, int maximum, String name) {
        if (value < 0 || value > maximum)
            throw new IllegalArgumentException(name + "须在 0%–" + maximum + "% 范围内");
    }
}
