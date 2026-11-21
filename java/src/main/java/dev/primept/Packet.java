package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import net.minecraft.client.model.geom.builders.UVPair;
import net.minecraft.client.resources.model.geometry.BakedQuad;

/** 可复用的小端捕获源包；只复制源字段，mesh 展开归 Rust。 */
final class Packet {
    private static final int MAX_BYTES = 256 * 1024 * 1024;
    private ByteBuffer bytes;
    Packet(){this(1<<20);}
    Packet(int initial){bytes=ByteBuffer.allocateDirect(initial).order(ByteOrder.LITTLE_ENDIAN);}
    private byte[] streamBuffer;

    void clear() { bytes.clear(); }
    int position() { return bytes.position(); }
    void position(int offset) { bytes.position(offset); }
    void putInt(int value) { reserve(4); bytes.putInt(value); }
    void putLong(long value) { reserve(8); bytes.putLong(value); }
    void putFloat(float value) { reserve(4); bytes.putFloat(value); }
    void putDouble(double value) { reserve(8); bytes.putDouble(value); }
    void putBytes(byte[] value) { reserve(value.length); bytes.put(value); }
    void putStream(java.io.InputStream input) throws java.io.IOException {
        int start = position();
        putInt(0);
        if (streamBuffer == null) streamBuffer = new byte[8192];
        int read;
        while ((read = input.read(streamBuffer)) != -1) {
            reserve(read);
            bytes.put(streamBuffer, 0, read);
        }
        patchInt(start, position() - start - 4);
    }
    void putBuffer(ByteBuffer value) { var copy=value.duplicate();reserve(copy.remaining());bytes.put(copy); }
    void putString(String value) {
        byte[] encoded = value.getBytes(java.nio.charset.StandardCharsets.UTF_8);
        putInt(encoded.length); putBytes(encoded);
    }
    void patchInt(int offset, int value) { bytes.putInt(offset, value); }
    ByteBuffer finish() { return bytes.flip(); }
    /** 资源字典发布后不再生成本包，解除对大块直接内存的引用。 */
    void discard() { bytes = null; }

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
        if (needed > MAX_BYTES) throw new IllegalStateException("捕获源包超过 256 MiB");
        int capacity = Math.min(MAX_BYTES, Math.max(bytes.capacity() * 2, needed));
        var larger = ByteBuffer.allocateDirect(capacity).order(ByteOrder.LITTLE_ENDIAN);
        larger.put(bytes.flip());
        bytes = larger;
    }
}
