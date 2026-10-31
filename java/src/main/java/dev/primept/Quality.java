package dev.primept;

import java.util.Locale;

/** RR 输入分辨率档位；wireId 与 native 边界一致。 */
public enum Quality {
    DLAA("dlaa", "DLAA", 0),
    QUALITY("quality", "质量", 1),
    BALANCED("balanced", "均衡", 2),
    PERFORMANCE("performance", "性能", 3),
    ULTRA_PERFORMANCE("ultra_performance", "超级性能", 4);

    private final String key;
    private final String label;
    private final int wireId;

    Quality(String key, String label, int wireId) {
        this.key = key;
        this.label = label;
        this.wireId = wireId;
    }

    public String key() { return key; }
    public String label() { return label; }
    public int wireId() { return wireId; }

    static Quality parse(String input) {
        String key = input.toLowerCase(Locale.ROOT);
        for (var quality : values()) if (quality.key.equals(key)) return quality;
        throw new IllegalArgumentException("质量档必须为 dlaa、quality、balanced、performance 或 ultra_performance");
    }
}
