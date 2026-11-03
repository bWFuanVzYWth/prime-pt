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
    private final MethodHandle destroy, reset, plan, profile, invalidateSection, invalidateAll, invalidateTints, failCapture, batch, sources, cameraMedium, tintBegin, tintRequests, tints, biomeWorld, biomeRequests, biomeSamples, atlas, draw, setSer, error;
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
        plan = bind(symbols, "prime_plan", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS,
                JAVA_INT, JAVA_INT, JAVA_INT, ADDRESS, JAVA_INT));
        profile = bind(symbols, "prime_profile", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_LONG,
                ADDRESS, ADDRESS, JAVA_INT, JAVA_INT));
        invalidateSection = bind(symbols, "prime_invalidate_section", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT, JAVA_INT, JAVA_INT));
        invalidateAll = bind(symbols, "prime_invalidate_all", FunctionDescriptor.of(JAVA_INT, JAVA_LONG));
        invalidateTints = bind(symbols,"prime_invalidate_tints",FunctionDescriptor.of(JAVA_INT,JAVA_LONG));
        failCapture = bind(symbols, "prime_fail_capture", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_LONG));
        setSer = bind(symbols, "prime_set_ser", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT));
        batch = bind(symbols, "prime_batch", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS, JAVA_LONG));
        sources = bind(symbols, "prime_sources", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, ADDRESS, JAVA_LONG));
        cameraMedium = bind(symbols, "prime_camera_medium", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT));
        tintRequests = bind(symbols,"prime_tint_requests",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_INT));
        tints = bind(symbols,"prime_tints",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_INT));
        tintBegin = bind(symbols,"prime_tint_begin",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,JAVA_INT));
        biomeWorld = bind(symbols,"prime_biome_world",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_LONG));
        biomeRequests = bind(symbols,"prime_biome_requests",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_INT));
        biomeSamples = bind(symbols,"prime_biome_samples",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_INT));
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

    int plan(ByteBuffer camera, int distance, int minSectionY, int maxSectionYExclusive, ByteBuffer requests) {
        try {
            int count = (int) plan.invokeExact(handle, MemorySegment.ofBuffer(camera),
                    distance, minSectionY, maxSectionYExclusive, MemorySegment.ofBuffer(requests), requests.remaining() / 24);
            if (count < 0) throw failure();
            return count;
        } catch (Throwable e) { throw unchecked(e); }
    }

    void profile(long elapsed, ByteBuffer stages, ByteBuffer output, int columns) {
        output.clear();
        try {
            int size = (int) profile.invokeExact(handle, elapsed, MemorySegment.ofBuffer(stages),
                    MemorySegment.ofBuffer(output), output.remaining(), columns);
            if (size < 0) throw failure();
            output.limit(size);
        } catch (Throwable e) { throw unchecked(e); }
    }

    void invalidateSection(int x, int y, int z) {
        try { check((int) invalidateSection.invokeExact(handle, x, y, z)); }
        catch (Throwable e) { throw unchecked(e); }
    }

    void invalidateAll() {
        try { check((int) invalidateAll.invokeExact(handle)); }
        catch (Throwable e) { throw unchecked(e); }
    }

    void invalidateTints(){try{check((int)invalidateTints.invokeExact(handle));}catch(Throwable e){throw unchecked(e);}}

    void failCapture(long request) {
        try { check((int) failCapture.invokeExact(handle, request)); }
        catch (Throwable e) { throw unchecked(e); }
    }

    /** 宿主已完成旧提交；仅选择资源阶段已准备的SER程序。 */
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

    void sources(ByteBuffer bytes) {
        try { check((int) sources.invokeExact(handle,MemorySegment.ofBuffer(bytes),(long)bytes.remaining())); }
        catch(Throwable e){throw unchecked(e);}
    }

    void cameraMedium(int kind) {
        try { check((int)cameraMedium.invokeExact(handle,kind)); }
        catch(Throwable e){throw unchecked(e);}
    }

    int tintRequests(ByteBuffer output) {
        try {
            int count=(int)tintRequests.invokeExact(handle,output==null?MemorySegment.NULL:MemorySegment.ofBuffer(output),output==null?0:output.remaining()/24);
            if(count<0)throw failure();return count;
        }catch(Throwable e){throw unchecked(e);}
    }

    void beginTints(int radius){try{check((int)tintBegin.invokeExact(handle,radius));}catch(Throwable e){throw unchecked(e);}}
    void biomeWorld(ByteBuffer bytes){try{check((int)biomeWorld.invokeExact(handle,MemorySegment.ofBuffer(bytes),(long)bytes.remaining()));}catch(Throwable e){throw unchecked(e);}}
    int biomeRequests(ByteBuffer output){
        try{int count=(int)biomeRequests.invokeExact(handle,output==null?MemorySegment.NULL:MemorySegment.ofBuffer(output),output==null?0:output.remaining()/24);if(count<0)throw failure();return count;}
        catch(Throwable e){throw unchecked(e);}
    }
    void biomeSamples(ByteBuffer ids){try{check((int)biomeSamples.invokeExact(handle,MemorySegment.ofBuffer(ids),ids.remaining()/4));}catch(Throwable e){throw unchecked(e);}}

    void tints(ByteBuffer colors) {
        try{check((int)tints.invokeExact(handle,MemorySegment.ofBuffer(colors),colors.remaining()/4));}
        catch(Throwable e){throw unchecked(e);}
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
