package dev.primept;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.nio.ByteBuffer;
import static java.lang.foreign.ValueLayout.*;

/** 同步 FFM 边界；CPU 包借用到返回，宿主图像必须保活至 GPU 提交完成。 */
final class Native implements AutoCloseable {
    private final Arena arena = Arena.ofConfined();
    private final MethodHandle destroy, reset, plan, profile, invalidateSection, invalidateAll, invalidateTints, invalidateBatch, failCapture, batch, sources, cameraMedium, tintBegin, tintRequests, tints, biomeWorld, biomeRequests, biomeSamples, atlas, draw, setSer, error;
    private final long handle;
    private final MethodHandle setUpdateBudget;
    private UpdateBudget updateBudget;
    private final MethodHandle setSolarOrbit;
    private SolarSettings solar;
    private final MethodHandle setDisplayOptions;
    private DisplaySettings display;
    private final MethodHandle setLightingOptions;
    private LightingSettings lighting;
    private boolean serRequested;
    private final MethodHandle setRendering;
    private final MethodHandle setRenderTuning;
    private RenderTuning tuning;
    private RenderMode renderMode;
    private RealtimeRenderer realtimeRenderer;

    Native(long instance, long physical, long device, long queue, int family, int queueIndex) {
        this(instance, physical, device, queue, family, queueIndex, false);
    }

    Native(long instance, long physical, long device, long queue, int family, int queueIndex, boolean ser) {
        this(instance, physical, device, queue, family, queueIndex, ser, true);
    }

    Native(long instance, long physical, long device, long queue, int family, int queueIndex, boolean ser, boolean requested) {
        SymbolLookup symbols;
        try {
            symbols = NativeRuntime.open(NativeRuntime.locate(), arena);
        } catch (RuntimeException | Error failure) {
            arena.close();
            throw failure;
        }
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
        invalidateBatch=bind(symbols,"prime_invalidate_batch",FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_LONG));
        failCapture = bind(symbols, "prime_fail_capture", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_LONG));
        setRendering = bind(symbols, "prime_set_rendering", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT, JAVA_INT));
        setRenderTuning = bindRenderTuning(symbols);
        setSer = bind(symbols, "prime_set_ser", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT));
        setUpdateBudget = bind(symbols, "prime_set_update_budget", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT));
        setSolarOrbit = bind(symbols, "prime_set_solar_orbit", FunctionDescriptor.of(JAVA_INT, JAVA_LONG, JAVA_INT, JAVA_INT));
        setDisplayOptions = bindDisplayOptions(symbols);
        setLightingOptions = bindLightingOptions(symbols);
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

    void rendering(RenderMode mode, RealtimeRenderer renderer) {
        if (mode == renderMode && renderer == realtimeRenderer) return;
        try {
            check((int) setRendering.invokeExact(handle, mode.wireId, renderer.wireId));
            renderMode = mode;
            realtimeRenderer = renderer;
        } catch (Throwable e) { throw unchecked(e); }
    }

    void reset() {
        try { check((int) reset.invokeExact(handle)); } catch (Throwable e) { throw unchecked(e); }
    }

    void updateRenderTuning(RenderTuning next) {
        if (next.equals(tuning)) return;
        try { check(invokeRenderTuning(setRenderTuning, handle, next)); tuning = next; }
        catch (Throwable failure) { throw unchecked(failure); }
    }

    static MethodHandle bindRenderTuning(SymbolLookup symbols) {
        return bind(symbols, "prime_set_render_tuning", FunctionDescriptor.of(JAVA_INT,
                JAVA_LONG, JAVA_INT, JAVA_FLOAT, JAVA_INT, JAVA_INT));
    }

    static int invokeRenderTuning(MethodHandle target, long id, RenderTuning options) throws Throwable {
        return (int) target.invokeExact(id, options.historyCap(), options.freshPercent()/100.0f,
                options.diagnostic() ? 1 : 0, options.restirCv() ? 1 : 0);
    }

    void updateBudget(UpdateBudget next) {
        if (updateBudget == next) return;
        try {
            check((int) setUpdateBudget.invokeExact(handle, next.wireId));
            updateBudget = next;
        } catch (Throwable e) { throw unchecked(e); }
    }

    void updateSolarOrbit(SolarSettings next) {
        if (next.equals(solar)) return;
        try {
            check((int) setSolarOrbit.invokeExact(handle, next.latitude(), next.longitude()));
            solar = next;
        } catch (Throwable e) { throw unchecked(e); }
    }

    void updateDisplayOptions(DisplaySettings next) {
        if (next.equals(display)) return;
        try {
            check(invokeDisplayOptions(setDisplayOptions, handle, next));
            display = next;
        } catch (Throwable e) { throw unchecked(e); }
    }

    static MethodHandle bindDisplayOptions(SymbolLookup symbols) {
        return bind(symbols, "prime_set_display_options", FunctionDescriptor.of(JAVA_INT, JAVA_LONG,
                JAVA_FLOAT, JAVA_FLOAT, JAVA_FLOAT, JAVA_FLOAT));
    }

    /** 生产和真实 FFM 夹具共用整数百分比到 native 归一化参数的调用点。 */
    static int invokeDisplayOptions(MethodHandle target, long id, DisplaySettings options) throws Throwable {
        return (int) target.invokeExact(id, options.autoExposure() / 100.0f, options.saturation() / 100.0f,
                options.hue() / 100.0f, options.compressionStart() / 100.0f);
    }

    void updateLightingOptions(LightingSettings next) {
        if (next.equals(lighting)) return;
        try {
            check(invokeLightingOptions(setLightingOptions, handle, next));
            lighting = next;
        } catch (Throwable e) { throw unchecked(e); }
    }

    static MethodHandle bindLightingOptions(SymbolLookup symbols) {
        return bind(symbols, "prime_set_lighting_options", FunctionDescriptor.of(JAVA_INT, JAVA_LONG,
                JAVA_FLOAT, JAVA_FLOAT));
    }

    static int invokeLightingOptions(MethodHandle target, long id, LightingSettings options) throws Throwable {
        return (int) target.invokeExact(id, options.sunEv(), options.blockEmissionEv());
    }

    int plan(ByteBuffer camera, int distance, int minSectionY, int maxSectionYExclusive, ByteBuffer requests) {
        try {
            int count = (int) plan.invokeExact(handle, MemorySegment.ofBuffer(camera),
                    distance, minSectionY, maxSectionYExclusive, MemorySegment.ofBuffer(requests), requests.remaining() / 24);
            if (count < 0) throw failure();
            return count;
        } catch (Throwable e) { throw unchecked(e); }
    }

    int profile(long elapsed, ByteBuffer stages, ByteBuffer output, int columns) {
        if(columns!=0)output.clear();
        try {
            int size = invokeProfile(profile,handle,elapsed,stages,output,columns);
            if (size < 0) throw failure();
            if(size>0)output.limit(size);
            return size;
        } catch (Throwable e) { throw unchecked(e); }
    }

    /** 仅记录时传空输出；生产和真实FFM验收共用显式静态类型的调用点。 */
    static int invokeProfile(MethodHandle target,long id,long elapsed,ByteBuffer stages,ByteBuffer output,int columns)throws Throwable{
        MemorySegment timings=MemorySegment.ofBuffer(stages);
        MemorySegment destination=columns==0?MemorySegment.NULL:MemorySegment.ofBuffer(output);
        int capacity=columns==0?0:output.remaining();
        return (int)target.invokeExact(id,elapsed,timings,destination,capacity,columns);
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
    /** 输入为已冻结PDI1；Native完整校验/应用成功后，调用者才确认事件所有权。 */
    void invalidateBatch(ByteBuffer bytes){try{check(invokeInvalidationBatch(invalidateBatch,handle,bytes));}catch(Throwable e){throw unchecked(e);}}
    /** 生产与真实FFM fixture共用调用点，显式固定指针/长度的静态类型。 */
    static int invokeInvalidationBatch(MethodHandle target,long id,ByteBuffer bytes)throws Throwable{
        MemorySegment source=bytes==null?MemorySegment.NULL:MemorySegment.ofBuffer(bytes);
        long length=bytes==null?0L:bytes.remaining();
        return (int)target.invokeExact(id,source,length);
    }

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
            // invokeExact以调用点静态类型匹配；条件指针须先固定为MemorySegment。
            MemorySegment destination = output == null ? MemorySegment.NULL : MemorySegment.ofBuffer(output);
            int capacity = output == null ? 0 : output.remaining() / 24;
            int count = (int) tintRequests.invokeExact(handle, destination, capacity);
            if (count < 0) throw failure();
            return count;
        } catch (Throwable e) { throw unchecked(e); }
    }

    void beginTints(int radius){try{check((int)tintBegin.invokeExact(handle,radius));}catch(Throwable e){throw unchecked(e);}}
    void biomeWorld(ByteBuffer bytes){try{check((int)biomeWorld.invokeExact(handle,MemorySegment.ofBuffer(bytes),(long)bytes.remaining()));}catch(Throwable e){throw unchecked(e);}}
    int biomeRequests(ByteBuffer output) {
        try {
            MemorySegment destination = output == null ? MemorySegment.NULL : MemorySegment.ofBuffer(output);
            int capacity = output == null ? 0 : output.remaining() / 24;
            int count = (int) biomeRequests.invokeExact(handle, destination, capacity);
            if (count < 0) throw failure();
            return count;
        } catch (Throwable e) { throw unchecked(e); }
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
