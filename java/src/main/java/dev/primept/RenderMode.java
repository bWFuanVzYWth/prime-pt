package dev.primept;

/** Prime 内部模式；离线冻结场景，实时继续消费世界更新。 */
public enum RenderMode {
    REALTIME("realtime", 0), OFFLINE("offline", 1);

    final String key;
    final int wireId;
    RenderMode(String key, int wireId) { this.key = key; this.wireId = wireId; }
    static RenderMode parse(String key) {
        for (var value : values()) if (value.key.equals(key)) return value;
        throw new IllegalArgumentException("渲染模式必须为 realtime 或 offline");
    }
}
