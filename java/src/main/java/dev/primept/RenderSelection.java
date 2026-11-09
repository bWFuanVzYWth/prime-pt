package dev.primept;

/** Mod 开关与实时／离线模式；两种模式均使用普通 PT。 */
record RenderSelection(boolean enabled, RenderMode mode) {
    static final RenderSelection DEFAULT = new RenderSelection(true, RenderMode.REALTIME);
    RenderSelection {
        if (mode == null) throw new IllegalArgumentException("渲染模式不能为空");
    }
    boolean offline() { return enabled && mode == RenderMode.OFFLINE; }
}
