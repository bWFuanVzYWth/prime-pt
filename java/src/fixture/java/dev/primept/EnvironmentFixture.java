package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import org.joml.Matrix4f;
import org.joml.Vector3f;

/** 太阳世界方向与 26.3 SkyRenderer 的变换一致；不启动游戏。 */
final class EnvironmentFixture {
    public static void main(String[] arguments) {
        var output = ByteBuffer.allocateDirect(20).order(ByteOrder.LITTLE_ENDIAN);
        for (float degrees : new float[] {0, 90, 180, 270}) {
            EnvironmentPacket.write(degrees, true, 63, output);
            var expected = new Matrix4f().rotateY((float) -Math.PI / 2)
                    .rotateX((float) Math.toRadians(degrees)).transformDirection(new Vector3f(0, 1, 0));
            near(output.getFloat(0), expected.x);
            near(output.getFloat(4), expected.y);
            near(output.getFloat(8), expected.z);
        }
        require(output.remaining() == 20 && output.getFloat(16) == 63, "环境 ABI 和海平面");
        EnvironmentPacket.write(0, false, 32, output);
        require(output.getFloat(0) == 0 && output.getFloat(4) == 0 && output.getFloat(8) == 0,
                "无太阳维度必须禁用大气");
        System.out.println("Solar transform/disabled dimension fixture passed");
    }

    private static void near(float actual, float expected) {
        require(Math.abs(actual - expected) < 1.0e-5f, "太阳方向: " + actual + " != " + expected);
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
