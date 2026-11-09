package dev.primept;

/** 实时原生单帧诊断，不改写 RR、SPP 或画质偏好。 */
record RenderTuning(boolean diagnostic) {
    static final RenderTuning DEFAULT = new RenderTuning(false);
    boolean diagnosticActive(RenderSelection selection) {
        return diagnostic && selection.enabled() && selection.mode() == RenderMode.REALTIME;
    }
}
