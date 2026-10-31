package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import net.minecraft.client.model.geom.builders.UVPair;
import net.minecraft.client.resources.model.geometry.BakedQuad;

/** 可复用的 PRT1 小端源包；只复制源字段，mesh 展开归 Rust。 */
final class Packet {
    private static final int MAX_BYTES = 256 * 1024 * 1024;
    private ByteBuffer bytes = ByteBuffer.allocateDirect(1 << 20).order(ByteOrder.LITTLE_ENDIAN);

    void clear() { bytes.clear(); }
    int position() { return bytes.position(); }
    void position(int offset) { bytes.position(offset); }
    void putInt(int value) { reserve(4); bytes.putInt(value); }
    void putFloat(float value) { reserve(4); bytes.putFloat(value); }
    void patchInt(int offset, int value) { bytes.putInt(offset, value); }
    ByteBuffer finish() { return bytes.flip(); }

    void quad(BakedQuad quad, int tint) {
        reserve(84);
        bytes.putInt(tint | 0xff000000);
        for (int i = 0; i < 4; i++) {
            var p = quad.position(i);
            long uv = quad.packedUV(i);
            bytes.putFloat(p.x()).putFloat(p.y()).putFloat(p.z());
            bytes.putFloat(UVPair.unpackU(uv)).putFloat(UVPair.unpackV(uv));
        }
    }

    private void reserve(int count) {
        if (bytes.remaining() >= count) return;
        int needed = Math.addExact(bytes.position(), count);
        if (needed > MAX_BYTES) throw new IllegalStateException("PRT1 源包超过 256 MiB");
        int capacity = Math.min(MAX_BYTES, Math.max(bytes.capacity() * 2, needed));
        var larger = ByteBuffer.allocateDirect(capacity).order(ByteOrder.LITTLE_ENDIAN);
        larger.put(bytes.flip());
        bytes = larger;
    }
}
