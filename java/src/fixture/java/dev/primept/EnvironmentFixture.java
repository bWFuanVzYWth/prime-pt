package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
/** 验证宿主只传输原版时角与天空开关。 */
final class EnvironmentFixture {
    public static void main(String[] arguments) {
        var output = ByteBuffer.allocateDirect(EnvironmentPacket.BYTES).order(ByteOrder.LITTLE_ENDIAN);
        for (float degrees : new float[] {-90, 0, 90, 180, 270, 360.25f}) {
            EnvironmentPacket.write(degrees, true, 63, output);
            require(output.getFloat(0) == degrees && output.getInt(12) == 1, "保留原始时角和天空标志");
        }
        require(output.remaining() == 16 && output.getFloat(8) == 63
                && output.getFloat(EnvironmentPacket.EXPOSURE_OFFSET) == 0, "环境 ABI 和海平面");
        EnvironmentPacket.write(30, false, 32, output);
        require(output.getFloat(0) == 0 && output.getFloat(8) == 32 && output.getInt(12) == 0, "无太阳维度禁用天空");
        EnvironmentPacket.write(Float.NaN, true, 63, output);
        require(output.getInt(12) == 0, "无效时角禁用天空");
        System.out.println("Solar hour angle/disabled dimension fixture passed");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
