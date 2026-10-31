package dev.primept;

import java.nio.ByteBuffer;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import org.joml.Matrix4f;
import org.joml.Vector3f;

/** 只在宿主透视投影就绪后写入 native Camera；进入世界时 initialized 可以先于投影有效。 */
final class CameraPacket {
    private CameraPacket() {}

    static boolean write(CameraRenderState camera, ByteBuffer output) {
        if (!camera.initialized) return false;
        float x = 1.0f / Math.abs(camera.projectionMatrix.m00());
        float y = 1.0f / Math.abs(camera.projectionMatrix.m11());
        float fov = (float) (2.0 * Math.atan(y));
        if (!Float.isFinite(x) || x <= 0 || !Float.isFinite(fov) || fov < 0.01f || fov >= 3.0f)
            return false;
        var inverse = new Matrix4f(camera.viewRotationMatrix).invert();
        if (!inverse.isFinite()) return false;
        var forward = inverse.transformDirection(new Vector3f(0, 0, -1)).normalize();
        var right = inverse.transformDirection(new Vector3f(1, 0, 0)).normalize().mul(x);
        var up = inverse.transformDirection(new Vector3f(0, 1, 0)).normalize().mul(y);
        output.clear();
        output.putDouble(camera.pos.x).putDouble(camera.pos.y).putDouble(camera.pos.z);
        vector(output, forward.x, forward.y, forward.z);
        vector(output, right.x, right.y, right.z);
        vector(output, up.x, up.y, up.z);
        output.flip();
        return true;
    }

    private static void vector(ByteBuffer output, float x, float y, float z) {
        output.putFloat(x).putFloat(y).putFloat(z).putFloat(0);
    }
}
