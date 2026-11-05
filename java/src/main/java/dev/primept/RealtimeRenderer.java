package dev.primept;

/** 实时积分器选择；离线始终使用独立普通 PT 累积。 */
public enum RealtimeRenderer {
    PATH_TRACE("path_trace", 0), RESTIR_PT("restir_pt", 1);

    final String key;
    final int wireId;
    RealtimeRenderer(String key, int wireId) { this.key = key; this.wireId = wireId; }
    static RealtimeRenderer parse(String key) {
        for (var value : values()) if (value.key.equals(key)) return value;
        throw new IllegalArgumentException("实时渲染器必须为 path_trace 或 restir_pt");
    }
}
