package dev.primept;

import java.nio.ByteBuffer;
import net.minecraft.client.Camera;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.world.attribute.EnvironmentAttributes;
import net.minecraft.world.level.dimension.DimensionType;

/** 16 B 环境输入，依次为原始太阳时角（度）、EV、海平面和天空开关。 */
final class EnvironmentPacket {
    static final int BYTES = 16;
    static final int EXPOSURE_OFFSET = 4;
    private EnvironmentPacket() {}

    static void write(ClientLevel level, Camera camera, float partial, ByteBuffer output) {
        boolean enabled = level != null && level.dimensionType().skybox() == DimensionType.Skybox.OVERWORLD;
        float angle = enabled ? camera.attributeProbe().getValue(EnvironmentAttributes.SUN_ANGLE, partial) : 0;
        write(angle, enabled, level == null ? 0 : level.getSeaLevel(), output);
    }

    /** 只捕获原版相同插值时刻的 SUN_ANGLE，太阳方向由 native 求出。 */
    static void write(float angleDegrees, boolean enabled, float seaLevel, ByteBuffer output) {
        if (!Float.isFinite(angleDegrees)) enabled = false;
        output.clear();
        output.putFloat(enabled ? angleDegrees : 0).putFloat(0).putFloat(seaLevel).putInt(enabled ? 1 : 0);
        output.flip();
    }
}
