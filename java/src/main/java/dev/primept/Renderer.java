package dev.primept;

import com.mojang.blaze3d.pipeline.RenderTarget;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.renderpearl.api.GpuFormat;
import com.mojang.renderpearl.api.device.GpuDeviceLossException;
import com.mojang.renderpearl.backend.vulkan.VulkanCommandEncoder;
import com.mojang.renderpearl.backend.vulkan.VulkanDevice;
import com.mojang.renderpearl.backend.vulkan.VulkanGpuTexture;
import com.mojang.renderpearl.backend.vulkan.VulkanGpuTextureView;
import dev.primept.mixin.DeviceAccess;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import net.minecraft.client.Camera;
import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import net.minecraft.core.BlockPos;
import org.joml.Vector4f;
import org.lwjgl.vulkan.VK10;

/** 渲染线程唯一所有者；显式完成宿主提交后才修改或销毁 native 资源。 */
public final class Renderer {
    /** 宿主尚无 storage usage，将该私有位映射到 VkImageUsageFlags。 */
    public static final int STORAGE = 32;
    private static final Capture CAPTURE = new Capture();
    private static final SourceChanges CHANGES = new SourceChanges();
    private static final ByteBuffer CAMERA = ByteBuffer.allocateDirect(72).order(ByteOrder.LITTLE_ENDIAN);
    private static final ByteBuffer REQUESTS = ByteBuffer.allocateDirect(64 * 24).order(ByteOrder.LITTLE_ENDIAN);
    private static final ByteBuffer ENVIRONMENT = ByteBuffer.allocateDirect(EnvironmentPacket.BYTES).order(ByteOrder.LITTLE_ENDIAN);
    private static final ByteBuffer STAGES = ByteBuffer.allocateDirect(FrameChart.STAGES * 8).order(ByteOrder.LITTLE_ENDIAN);
    private static final ByteBuffer PROFILE = ByteBuffer.allocateDirect(FrameChart.CAPACITY).order(ByteOrder.LITTLE_ENDIAN);
    private static Native renderer;
    private static boolean active;
    private static ClientLevel vanillaRestore;
    private static boolean hasFrame;
    private static boolean frozen;
    private static VulkanCommandEncoder encoder;
    private static boolean faultReported;
    private static long generation = -1;
    private static volatile boolean resetRequested;
    private static long lastCompleted;
    private static ByteBuffer tintRequests=ByteBuffer.allocateDirect(24*1024).order(ByteOrder.LITTLE_ENDIAN);
    private static ByteBuffer biomeRequests=ByteBuffer.allocateDirect(24*1024).order(ByteOrder.LITTLE_ENDIAN);
    private static ClientLevel biomeWorld;
    private Renderer() {}

    /** ModelManager完成首次/重载发布时同步准备整代源及GPU产物；不由首次命中懒构建。 */
    public static void prepareResources(){
        RenderSystem.assertOnRenderThread();
        if (!ensureRenderer()) return;
        await();
        renderer.rendering(RenderMode.REALTIME);
        frozen=false;hasFrame=false;
        var next=SourceResources.prepareReload(Minecraft.getInstance());
        var models=FeatureModels.prepare(Minecraft.getInstance().getModelManager().entityModels().get());
        Atlas.send(renderer);renderer.sources(next.packet(),models.packet());models.publish();next.accepted();Atlas.accepted();generation=next.generation;biomeWorld=null;CHANGES.clear();
    }

    /** 在外层提取边界交接原版与 Prime；完成证明后才改变 native 模式。 */
    public static void beginFrame() {
        SupportNotice.show();
        boolean enabled = Settings.enabled() && renderer != null;
        if (active != enabled) {
            if (renderer != null) {
                await();
                renderer.rendering(RenderMode.REALTIME);
                if (!enabled) renderer.reset();
            }
            active = enabled;
            frozen = false;
            hasFrame = false;
            lastCompleted = 0;
            FrameChart.clear();
            resetRequested = true;
            CHANGES.clear();
            var minecraft = Minecraft.getInstance();
            if (!enabled) {
                WorldFeatures.releaseWorld();
                if (minecraft.level != null)
                    ((TerrainSnapshot.Cache)minecraft.level.getChunkSource()).primept$releaseBlockEntities();
            }
            vanillaRestore = active ? null : minecraft.level;
            minecraft.levelExtractor.allChanged();
        }
    }

    public static boolean frozen() { return frozen; }
    public static boolean active() { return active; }

    /** 在原版新建可见性图之后恢复现有区块；不向共享事件日志重复投递。 */
    public static void restoreVanilla(ClientLevel level, net.minecraft.client.renderer.SectionOcclusionGraph graph) {
        if (!active && level != null && vanillaRestore == level) {
            ((TerrainSnapshot.Cache) level.getChunkSource()).primept$restoreTerrain(graph);
            vanillaRestore = null;
        }
    }

    private static boolean ensureRenderer(){
        if(renderer!=null)return true;
        var backend=((DeviceAccess)RenderSystem.getDevice()).primept$backend();
        if(!(backend instanceof VulkanDevice device)) {
            SupportNotice.openGl();
            return false;
        }
        if (!Bootstrap.rayTracingEnabled()) return false;
        encoder=device.createCommandEncoder();await();
        renderer=new Native(device.instance().vkInstance().address(),device.vkDevice().getPhysicalDevice().address(),device.vkDevice().address(),device.graphicsQueue().vkQueue().address(),device.graphicsQueue().queueFamilyIndex(),Bootstrap.graphicsQueueIndex(),Bootstrap.invocationReorder(device.vkDevice()),Settings.requestedSer(),Bootstrap.diagnosticCheckpoints(device.vkDevice()),Bootstrap.opacityMicromap(device.vkDevice()));
        faultReported = false;
        return true;
    }

    private static void completeTints(Minecraft minecraft){
        renderer.beginTints(minecraft.options.biomeBlendRadius().get());
        int pages=renderer.biomeRequests(null);
        if(pages>0){
            int needed=Math.multiplyExact(pages,24);
            if(needed>256*1024*1024)throw new IllegalStateException("biome请求超过源包预算");
            if(biomeRequests.capacity()<needed)biomeRequests=ByteBuffer.allocateDirect(needed).order(ByteOrder.LITTLE_ENDIAN);
            biomeRequests.clear();biomeRequests.limit(needed);
            if(renderer.biomeRequests(biomeRequests)!=pages)throw new IllegalStateException("同步biome阶段数量变化");
            renderer.biomeSamples(SourceResources.prepared().biomes.respond(minecraft.level,biomeRequests,pages));
        }
        int count=renderer.tintRequests(null);if(count==0)return;
        int bytes=Math.multiplyExact(count,24);
        if(bytes>256*1024*1024)throw new IllegalStateException("tint请求超过源包预算");
        if(tintRequests.capacity()<bytes)tintRequests=ByteBuffer.allocateDirect(bytes).order(ByteOrder.LITTLE_ENDIAN);
        tintRequests.clear();tintRequests.limit(bytes);
        if(renderer.tintRequests(tintRequests)!=count)throw new IllegalStateException("同步tint阶段数量变化");
        renderer.tints(CAPTURE.tints(minecraft,tintRequests,count));
    }

    /** 替代原版世界绘制；相机、手持物和 UI 继续使用宿主路径。 */
    public static void render(CameraRenderState camera, RenderTarget target) {
        try { renderFrame(camera, target); }
        catch (GpuDeviceLossException failure) { reportDeviceLoss(failure); throw failure; }
    }

    private static void renderFrame(CameraRenderState camera, RenderTarget target) {
        var minecraft = Minecraft.getInstance();
        if (minecraft.level == null || target.width <= 0 || target.height <= 0) {
            lastCompleted = 0; FrameChart.clear(); return;
        }
        RenderSystem.assertOnRenderThread();
        var host = RenderSystem.getDevice().createCommandEncoder();
        host.clearDepthTexture(target.getDepthTexture(), 1.0);
        if (!Atlas.ready() || !CameraPacket.write(camera, CAMERA)) {
            lastCompleted = 0; FrameChart.clear();
            host.clearColorTexture(target.getColorTexture(), new Vector4f(0.2f, 0.4f, 0.7f, 1));
            return;
        }
        for (int i = 0; i < FrameChart.STAGES; i++) STAGES.putLong(i * 8, 0);
        if(renderer==null)throw new IllegalStateException("首次资源加载尚未准备Prime资源所有者");
        var updateBudget = Settings.updateBudget();
        boolean resetWorld = CAPTURE.select(minecraft.level) || resetRequested;
        boolean resume = frozen && !Settings.offline();
        ByteBuffer features=null;
        if (!Settings.offline()||!hasFrame||resetWorld) {
            ((WorldFeatures.SourceRenderer)minecraft.levelRenderer).primept$features(camera);
            features=WorldFeatures.packet(camera);
        }
        long started = System.nanoTime();
        try (var fence = encoder.createFence()) {
            encoder.submit();
            long submitting = System.nanoTime() - started;
            try {
                // 同一帧内先推进纯CPU阶段。重置、程序切换和绘制才消费GPU完成保证。
                if (!resetWorld && !resume && !(Settings.offline() && hasFrame)) prepareFrame(minecraft, updateBudget);
            } finally {
                long waiting = System.nanoTime();
                if (!fence.awaitCompletion(5_000_000_000L)) throw fenceTimeout();
                STAGES.putLong(0, submitting + System.nanoTime() - waiting);
            }
        }
        long stage = System.nanoTime();
        renderer.rendering(resetWorld ? RenderMode.REALTIME : Settings.mode());
        renderer.updateRenderTuning(Settings.tuning());
        frozen = !resetWorld && Settings.offline() && hasFrame;
        if (!frozen) renderer.ser(Settings.requestedSer());
        if (resetWorld) {
            hasFrame = false;
            frozen = false;
            renderer.reset();
            biomeWorld=null;
            resetRequested = false;
            lastCompleted = 0;
            FrameChart.clear();
        }
        STAGES.putLong(8, STAGES.getLong(8) + System.nanoTime() - stage);
        if (resetWorld) {
            prepareFrame(minecraft, updateBudget);
            renderer.rendering(Settings.mode());
        } else if (resume) prepareFrame(minecraft, updateBudget);
        renderer.updateDisplayOptions(Settings.display());
        int medium=switch(minecraft.gameRenderer.mainCamera().getFluidInCamera()){case WATER->1;case LAVA->2;default->0;};
        if (!frozen) renderer.cameraMedium(medium);
        if (!(target.getColorTexture() instanceof VulkanGpuTexture texture)
                || !(target.getColorTextureView() instanceof VulkanGpuTextureView view)
                || texture.getFormat() != GpuFormat.RGBA8_UNORM || (texture.usage() & STORAGE) == 0)
            throw new IllegalStateException("Prime PT 目标必须是支持 storage 的 RGBA8_UNORM");
        stage = System.nanoTime();
        var command = encoder.allocateAndBeginTransientCommandBuffer();
        ENVIRONMENT.putFloat(EnvironmentPacket.EXPOSURE_OFFSET, Settings.exposure());
        renderer.draw(command.address(), texture.vkImage(), view.vkImageView(), target.width, target.height,
                CAMERA, ENVIRONMENT, features, Settings.activeDenoiser(), Settings.quality(), Settings.samples(), Settings.bounces());
        hasFrame = true;
        int status = VK10.vkEndCommandBuffer(command);
        if (status != VK10.VK_SUCCESS) throw new IllegalStateException("vkEndCommandBuffer: " + status);
        STAGES.putLong(88, System.nanoTime() - stage);
        stage = System.nanoTime();
        encoder.execute(command);
        STAGES.putLong(96, System.nanoTime() - stage);
        long completed = System.nanoTime();
        long elapsed = lastCompleted == 0 || minecraft.isPaused() ? 0 : completed - lastCompleted;
        lastCompleted = completed;
        if(renderer.profile(elapsed, STAGES, PROFILE, FrameChart.takeColumns())>0)FrameChart.update(PROFILE);
    }

    /** 仅更新 CPU 世界、捕获票据和 tint 候选，不访问 GPU 所有者。 */
    private static void prepareFrame(Minecraft minecraft, UpdateBudget updateBudget) {
        long stage = System.nanoTime();
        renderer.updateBudget(updateBudget);
        renderer.updateSolarOrbit(Settings.solar());
        renderer.updateLightingOptions(Settings.lighting());
        renderer.updateDisplayOptions(Settings.display());
        if(biomeWorld!=minecraft.level){renderer.biomeWorld(SourceResources.prepared().biomes.world(minecraft.level));biomeWorld=minecraft.level;}
        var invalidations=CHANGES.batch(minecraft.level,1024);
        if(invalidations!=null){renderer.invalidateBatch(invalidations);CHANGES.accepted();}
        STAGES.putLong(8, STAGES.getLong(8) + System.nanoTime() - stage);
        stage = System.nanoTime();
        REQUESTS.clear().limit(updateBudget.captures * 24);
        int count = renderer.plan(CAMERA, minecraft.options.getEffectiveRenderDistance(),
                minecraft.level.getMinSectionY(), minecraft.level.getMaxSectionY() + 1, REQUESTS);
        STAGES.putLong(16, System.nanoTime() - stage);
        if (count > 0) {
            try {
                stage = System.nanoTime();
                var packet = CAPTURE.next(minecraft, REQUESTS, count);
                STAGES.putLong(24, System.nanoTime() - stage);
                stage = System.nanoTime();
                renderer.batch(packet);
                STAGES.putLong(32, System.nanoTime() - stage);
            }
            catch (RuntimeException | Error failure) {
                for (int i = 0; i < count; i++) {
                    try { renderer.failCapture(REQUESTS.getLong(i * 24)); }
                    catch (RuntimeException | Error cleanup) { failure.addSuppressed(cleanup); }
                }
                throw failure;
            }
        }
        long tintStarted=System.nanoTime();completeTints(minecraft);STAGES.putLong(8,STAGES.getLong(8)+System.nanoTime()-tintStarted);
    }

    private static void await() {
        try (var fence = encoder.createFence()) {
            encoder.submit();
            if (!fence.awaitCompletion(5_000_000_000L)) throw fenceTimeout();
        } catch (GpuDeviceLossException failure) {
            reportDeviceLoss(failure);
            throw failure;
        }
    }

    private static void reportDeviceLoss(GpuDeviceLossException failure) {
        if (faultReported || renderer == null) return;
        faultReported = true;
        try { renderer.reportDeviceLoss(); }
        catch (RuntimeException | Error diagnosticFailure) { failure.addSuppressed(diagnosticFailure); }
    }

    private static IllegalStateException fenceTimeout() {
        return new IllegalStateException("Prime PT 宿主 GPU 提交未完成，资源保留到进程退出");
    }

    /** 世界替换延迟到下次 render 的完成边界处理。 */
    public static void reset() { resetRequested = true; vanillaRestore = null; }
    /** tint 和模型源刷新保留旧覆盖，分步重捕获；不切换世界域。 */
    public static void invalidate(ClientLevel level) { if (active) CHANGES.all(level); }
    /** 与宿主天空使用同一个相机 attribute probe 和插值时刻。 */
    public static void environment(ClientLevel level, Camera camera, float partial) {
        if (frozen) return;
        EnvironmentPacket.write(level, camera, partial, ENVIRONMENT);
    }
    /** 仅捕获实际block源范围；邻域及slab依赖由Rust统一推导一次。 */
    public static void dirty(ClientLevel level, BlockPos pos) {
        if (active) CHANGES.block(level,pos.getX(),pos.getY(),pos.getZ());
    }
    /** chunk 加载和卸载统一替换对应 section 源。 */
    public static void column(ClientLevel level, int x, int z) {
        if (active) CHANGES.column(level,x,z,level.getMinSectionY(),level.getMaxSectionY()+1,true);
    }
    /** 已加载chunk收到独立biome数据，不清其压缩block源。 */
    public static void biomeColumn(ClientLevel level,int x,int z){if (active) CHANGES.biome(level,x,z);}
    /** 必须在宿主 VulkanDevice.close 之前调用。 */
    public static void close() {
        vanillaRestore = null;
        if (renderer == null) return;
        await(); renderer.close(); renderer = null; generation = -1; CAPTURE.invalidate(); resetRequested = false;biomeWorld=null;
        lastCompleted = 0; FrameChart.clear(); frozen = false; hasFrame = false;
    }
}
