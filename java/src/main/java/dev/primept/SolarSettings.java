package dev.primept;

/** 旧 Prime 的观测纬度与太阳黄经，均使用整数度。 */
record SolarSettings(int latitude, int longitude) {
    static final SolarSettings DEFAULT = new SolarSettings(30, 0);

    SolarSettings {
        if (latitude < -90 || latitude > 90 || longitude < 0 || longitude >= 360)
            throw new IllegalArgumentException("纬度须在 −90..=90°，太阳黄经须在 0..=359°");
    }
}
