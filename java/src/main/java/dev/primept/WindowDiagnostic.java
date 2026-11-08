package dev.primept;

/** 显式诊断运行中的窗口/WSI边界；BEGIN没有对应END即定位阻塞调用。 */
public final class WindowDiagnostic {
    private static final boolean ENABLED = Boolean.getBoolean("primept.windowDiagnostic");
    private WindowDiagnostic() {}
    public static void mark(String stage) {
        if (ENABLED) System.getLogger("prime_pt").log(System.Logger.Level.INFO,
                "window_transition " + stage + " thread=" + Thread.currentThread().getName());
    }
}
