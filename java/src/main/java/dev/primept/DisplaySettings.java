package dev.primept;

/** 呈现参数。曝光和 DRT 以整数百分比保存，关闭泛光时保留所选滤波器。 */
record DisplaySettings(int autoExposure, int saturation, int hue, int compressionStart, boolean lensBloom, BloomMode bloomMode) {
    static final int MAX_AUTO_EXPOSURE = 100;
    static final int MAX_SATURATION = 50;
    static final int MAX_HUE = 100;
    static final int MAX_COMPRESSION_START = 99;
    static final DisplaySettings DEFAULT = new DisplaySettings(65, 5, 75, 40, true, BloomMode.FAST_CIRCULAR);

    DisplaySettings {
        java.util.Objects.requireNonNull(bloomMode);
        requirePercent(autoExposure, MAX_AUTO_EXPOSURE, "自动曝光强度");
        requirePercent(saturation, MAX_SATURATION, "饱和度补偿");
        requirePercent(hue, MAX_HUE, "色相补偿");
        requirePercent(compressionStart, MAX_COMPRESSION_START, "压缩起点");
    }

    int bloomWireId() { return lensBloom ? bloomMode.wireId : 0; }

    private static void requirePercent(int value, int maximum, String name) {
        if (value < 0 || value > maximum)
            throw new IllegalArgumentException(name + "须在 0%–" + maximum + "% 范围内");
    }
}
