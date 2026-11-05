package dev.primept;

/** 固定ReSTIR输出概率、历史上限与实时原生诊断；不会改写RR、SPP或画质偏好。 */
record RenderTuning(int historyCap, int freshPercent, boolean diagnostic, boolean restirCv) {
    static final RenderTuning DEFAULT = new RenderTuning(20, 40, false, true);

    RenderTuning {
        if (historyCap < 1 || historyCap > 100)
            throw new IllegalArgumentException("历史M上限必须为1..100");
        if (freshPercent < 0 || freshPercent > 100)
            throw new IllegalArgumentException("fresh输出比例必须为0..100%");
    }

    boolean diagnosticActive(RenderSelection selection) {
        return diagnostic && selection.enabled() && selection.mode() == RenderMode.REALTIME;
    }
}
