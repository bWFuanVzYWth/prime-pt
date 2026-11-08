package dev.primept;

/** 首表面界面反射／进入内部的重建采样策略；不改变后续反弹或离线采样。 */
enum PrimarySampling {
    RANDOM(1, "random"), SEPARATED(2, "separated");

    final int samples;
    final String key;

    PrimarySampling(int samples, String key) {
        this.samples = samples;
        this.key = key;
    }

    static PrimarySampling fromSamples(int samples) {
        return samples <= 1 ? RANDOM : SEPARATED;
    }
}
