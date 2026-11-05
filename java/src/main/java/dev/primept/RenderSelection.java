package dev.primept;

/** 保留用户选择；有效后端按 Mod 开关、模式、实时积分器依次决定。 */
record RenderSelection(boolean enabled, RenderMode mode, RealtimeRenderer renderer) {
    static final RenderSelection DEFAULT = new RenderSelection(true, RenderMode.REALTIME, RealtimeRenderer.PATH_TRACE);
    RenderSelection {
        if (mode == null || renderer == null) throw new IllegalArgumentException("渲染模式和实时渲染器不能为空");
    }
    boolean offline() { return enabled && mode == RenderMode.OFFLINE; }
}
