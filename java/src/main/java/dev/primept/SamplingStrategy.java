package dev.primept;

/** 实时采样档位：首面覆盖与路径轮盘共同选择；离线批大小独立解释。 */
enum SamplingStrategy {
    PERFORMANCE(1, "performance"), QUALITY(2, "quality");

    final int samples;
    final String key;

    SamplingStrategy(int samples, String key) {
        this.samples = samples;
        this.key = key;
    }

    static SamplingStrategy fromSamples(int samples) {
        return samples <= 1 ? PERFORMANCE : QUALITY;
    }

    static SamplingStrategy parse(String key) {
        return switch (key) {
            case "performance" -> PERFORMANCE;
            case "quality" -> QUALITY;
            default -> throw new IllegalArgumentException("采样策略必须为 performance 或 quality");
        };
    }
}
