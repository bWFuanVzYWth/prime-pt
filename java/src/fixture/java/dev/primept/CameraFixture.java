package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.Vec3;

/** 验证首帧投影等待，以及有效相机的中心方向和视锥尺寸；不启动游戏。 */
final class CameraFixture {
    public static void main(String[] arguments) {
        var camera = new CameraRenderState();
        var output = ByteBuffer.allocateDirect(72).order(ByteOrder.LITTLE_ENDIAN);
        require(!CameraPacket.write(camera, output), "未初始化相机不得提交");
        camera.initialized = true;
        camera.projectionMatrix.perspective(0, 16.0f / 9, 0.05f, 1000);
        require(!CameraPacket.write(camera, output), "进入世界的零 FOV 不得提交");
        camera.projectionMatrix.zero();
        require(!CameraPacket.write(camera, output), "空投影不得提交");
        camera.pos = new Vec3(30_000_000.125, 34, -30_000_000.25);
        camera.viewRotationMatrix.rotationY((float) Math.PI / 2);
        camera.projectionMatrix.setPerspective((float) Math.PI / 2, 2, 0.05f, 1000);
        require(CameraPacket.write(camera, output), "有效透视相机应可提交");
        require(output.remaining() == 72, "native WorldCamera 长度");
        require(output.getDouble(0) == camera.pos.x && output.getDouble(8) == 34 && output.getDouble(16) == camera.pos.z, "绝对相机保留 f64 分数");
        near(output.getFloat(24), 1, "中心射线世界方向");
        near(output.getFloat(28), 0, "中心射线竖直分量");
        near(output.getFloat(32), 0, "中心射线深度分量");
        near(output.getFloat(48), 2, "水平半视锥");
        near(output.getFloat(60), 1, "竖直半视锥");
        camera.projectionMatrix.m11(-camera.projectionMatrix.m11());
        require(CameraPacket.write(camera, output), "Vulkan 翻转投影应可提交");
        near(output.getFloat(60), 1, "世界向上与投影 Y 符号无关");
        var current = new Vec3(30_000_001, 64, 0);
        var rendered = new Vec3(30_000_000, 64, 0);
        var bounds = new AABB(current.x - 0.3, 64, -0.3, current.x + 0.3, 65.8, 0.3);
        for (int side : new int[]{-1, 1}) {
            require(WorldFeatures.intersectsCamera(rendered.add(0, 1.62, side * 0.32), bounds, current, rendered),
                    "第三人称相机被墙推回正在插值的玩家时应隐藏自身直视");
            require(!WorldFeatures.intersectsCamera(rendered.add(0, 1.62, side * 4), bounds, current, rendered),
                    "前、后第三人称在常规距离应看见玩家");
        }
        System.out.println("Camera readiness/projection and observer visibility fixture passed");
    }

    private static void near(float actual, float expected, String message) {
        require(Math.abs(actual - expected) < 1.0e-5f, message + ": " + actual);
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
