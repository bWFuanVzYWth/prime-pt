package dev.primept;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.nio.ByteBuffer;
import java.nio.file.Path;
import static java.lang.foreign.ValueLayout.*;

/** 同步 FFM 边界；CPU 包借用到返回，宿主图像必须保活至 GPU 提交完成。 */
final class Native implements AutoCloseable {
    private final Arena arena = Arena.ofConfined();
    private final MethodHandle destroy, reset, batch, atlas, draw, setSer, error;
    private final long handle;
    private boolean serRequested;

    Native(long instance, long physical, long device, long queue, int family, int queueIndex) {
        this(instance, physical, device, queue, family, queueIndex, false);
    }

    Native(long instance, long physical, long device, long queue, int family, int queueIndex, boolean ser) {
        this(instance, physical, device, queue, family, queueIndex, ser, true);
    }

    Native(long instance, long physical, long device, long queue, int family, int queueIndex, boolean ser, boolean requested) {
        var symbols = SymbolLookup.libraryLookup(Path.of(System.getProperty("primept.native")), arena);
        var create = ser
                ? bind(symbols, "prime_create_options", FunctionDescriptor.of(JAVA_LONG,
                        JAVA_LONG, JAVA_LONG, JAVA_LONG, JAVA_LONG, JAVA_INT, JAVA_INT, JAVA_INT))
                : bind(symbols, "prime_create", FunctionDescriptor.of(JAVA_LONG,
                        JAVA_LONG, JAVA_LONG, JAVA_LONG, JAVA_LONG, JAVA_INT, JAVA_INT));
        destroy = bind(symbols, "prime_destroy", FunctionDescriptor.of(JAVA_INT, JAVA_LONG));
        reset = bind(symbols, "prime_reset", FunctionDescriptor.of(JAVA_INT, JAVA_LONG));
        setSer = bind(symbols, "prime_set_ser", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT));
        batch = bind(symbols, "prime_batch", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS, JAVA_LONG));
        atlas = bind(symbols, "prime_atlas", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS, JAVA_LONG, JAVA_INT, JAVA_INT));
        draw = bind(symbols, "prime_draw", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_LONG,
                JAVA_LONG, JAVA_LONG, JAVA_INT, JAVA_INT, ADDRESS, ADDRESS, JAVA_INT, JAVA_INT, JAVA_INT, JAVA_INT));
        error = bind(symbols, "prime_error", FunctionDescriptor.of(ADDRESS));
        try {
            handle = ser
                    ? (long) create.invokeExact(instance, physical, device, queue, family, queueIndex, requested ? 1 : 3)
                    : (long) create.invokeExact(instance, physical, device, queue, family, queueIndex);
            if (handle == 0) throw failure();
            serRequested = requested;
        } catch (Throwable e) { arena.close(); throw unchecked(e); }
    }

    private static MethodHandle bind(SymbolLookup symbols, String name, FunctionDescriptor type) {
        return Linker.nativeLinker().downcallHandle(symbols.find(name).orElseThrow(), type);
    }

    void reset() {
        try { check((int) reset.invokeExact(handle)); } catch (Throwable e) { throw unchecked(e); }
    }

    /** 宿主已完成旧提交；只有偏好改变时才重建阶段程序。 */
    void ser(boolean requested) {
        if (serRequested == requested) return;
        try {
            check((int) setSer.invokeExact(handle, requested ? 1 : 0));
            serRequested = requested;
        } catch (Throwable e) { throw unchecked(e); }
    }

    void batch(ByteBuffer bytes) {
        try { check((int) batch.invokeExact(handle, MemorySegment.ofBuffer(bytes), (long) bytes.remaining())); }
        catch (Throwable e) { throw unchecked(e); }
    }

    void atlas(byte[] rgba, int width, int height) {
        try (var temporary = Arena.ofConfined()) {
            var pixels = temporary.allocate(rgba.length);
            pixels.copyFrom(MemorySegment.ofArray(rgba));
            check((int) atlas.invokeExact(handle, pixels, (long) rgba.length, width, height));
        } catch (Throwable e) { throw unchecked(e); }
    }

    void draw(long command, long image, long view, int width, int height, ByteBuffer camera, ByteBuffer environment,
            boolean rr, Quality quality, int samples, int bounces) {
        try { check((int) draw.invokeExact(handle, command, image, view, width, height,
                MemorySegment.ofBuffer(camera), MemorySegment.ofBuffer(environment), rr ? 1 : 0,
                quality.wireId(), samples, bounces)); }
        catch (Throwable e) { throw unchecked(e); }
    }

    private void check(int status) { if (status != 0) throw failure(); }

    private IllegalStateException failure() {
        try {
            var message = (MemorySegment) error.invokeExact();
            return new IllegalStateException(message.address() == 0 ? "native RT 失败" : message.reinterpret(4096).getString(0));
        } catch (Throwable e) { return new IllegalStateException("读取 native 错误失败", e); }
    }

    private static RuntimeException unchecked(Throwable e) {
        if (e instanceof RuntimeException runtime) return runtime;
        if (e instanceof Error fatal) throw fatal;
        return new IllegalStateException("FFM 调用失败", e);
    }

    @Override public void close() {
        try { check((int) destroy.invokeExact(handle)); } catch (Throwable e) { throw unchecked(e); }
        arena.close();
    }
}
