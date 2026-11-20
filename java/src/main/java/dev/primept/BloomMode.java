package dev.primept;

/** 呈现滤波器的配置键和 native 模式值。关闭泛光使用独立的 0。 */
enum BloomMode {
    CAMERA("camera", 3), PUPIL("pupil", 4), FAST_CIRCULAR("fast_circular", 5);

    final String key;
    final int wireId;

    BloomMode(String key, int wireId) {
        this.key = key;
        this.wireId = wireId;
    }

    static BloomMode parse(String key) {
        return switch (key) {
            case "chamfered" -> CAMERA;
            case "iris" -> PUPIL;
            default -> {
                for (var mode : values()) if (mode.key.equals(key)) yield mode;
                throw new IllegalArgumentException("未知泛光模式 " + key);
            }
        };
    }
}
