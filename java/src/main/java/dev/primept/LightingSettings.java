package dev.primept;

/** 光源强度保留旧版四分之一 EV 整数配置。 */
record LightingSettings(int sunEvQuarters, int blockEmissionEvQuarters) {
    static final int SUN_LIMIT = 32;
    static final int BLOCK_EMISSION_LIMIT = 40;
    static final float EV_STEP = 0.25f;
    static final LightingSettings DEFAULT = new LightingSettings(0, 0);

    LightingSettings {
        requireRange(sunEvQuarters, SUN_LIMIT, "太阳强度");
        requireRange(blockEmissionEvQuarters, BLOCK_EMISSION_LIMIT, "方块发光强度");
    }

    float sunEv() { return sunEvQuarters * EV_STEP; }
    float blockEmissionEv() { return blockEmissionEvQuarters * EV_STEP; }

    private static void requireRange(int value, int limit, String name) {
        if (value < -limit || value > limit)
            throw new IllegalArgumentException(name + "须在 ±" + (limit * EV_STEP) + " EV 内");
    }
}
