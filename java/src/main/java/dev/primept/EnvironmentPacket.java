package dev.primept;

import java.nio.ByteBuffer;
import net.minecraft.client.Camera;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.world.attribute.EnvironmentAttributes;
import net.minecraft.world.level.dimension.DimensionType;

/** 20 B 环境输入：朝向太阳的世界单位向量、EV、海平面；零向量禁用太阳和大气。 */
final class EnvironmentPacket {
    private EnvironmentPacket() {}

    static void write(ClientLevel level, Camera camera, float partial, ByteBuffer output) {
        boolean enabled = level != null && level.dimensionType().skybox() == DimensionType.Skybox.OVERWORLD;
        float angle = enabled ? camera.attributeProbe().getValue(EnvironmentAttributes.SUN_ANGLE, partial) : 0;
        write(angle, enabled, level == null ? 0 : level.getSeaLevel(), output);
    }

    /** 26.3 SkyRenderer：SUN_ANGLE 为度，太阳顶点经过 RY(-90°) RX(angle)。 */
    static void write(float angleDegrees, boolean enabled, float seaLevel, ByteBuffer output) {
        if (!Float.isFinite(angleDegrees)) enabled = false;
        double angle = Math.toRadians(angleDegrees);
        output.clear();
        output.putFloat(enabled ? (float) -Math.sin(angle) : 0);
        output.putFloat(enabled ? (float) Math.cos(angle) : 0).putFloat(0);
        output.putFloat(0).putFloat(seaLevel);
        output.flip();
    }
}
