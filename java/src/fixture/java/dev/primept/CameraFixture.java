package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import net.minecraft.client.renderer.state.level.CameraRenderState;
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
        camera.hudFov = 70;
        var hand = new org.joml.Vector4f(0.3f, -0.2f, -0.7f, 1);
        var hud = new org.joml.Matrix4f().setPerspective((float)Math.toRadians(camera.hudFov), 2f, 0.05f, 1000f);
        hud.m11(Math.copySign(hud.m11(), camera.projectionMatrix.m11()));
        var expected = hud.transform(new org.joml.Vector4f(hand));
        var world = CameraPacket.handTransform(camera, 3840, 1920).transform(new org.joml.Vector4f(hand));
        var view = camera.viewRotationMatrix.transform(world);
        near(view.z, hand.z, "手持灯保留世界距离");
        var actual = camera.projectionMatrix.transform(view);
        near(actual.x/actual.w, expected.x/expected.w, "手持物水平投影");
        near(actual.y/actual.w, expected.y/expected.w, "手持物垂直投影");
        System.out.println("Camera readiness/projection and hand projection fixture passed");
    }

    private static void near(float actual, float expected, String message) {
        require(Math.abs(actual - expected) < 1.0e-5f, message + ": " + actual);
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
