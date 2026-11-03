package dev.primept;

import com.mojang.blaze3d.pipeline.RenderTarget;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.renderpearl.api.GpuFormat;
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
import net.minecraft.core.Direction;
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
    private static final ByteBuffer ENVIRONMENT = ByteBuffer.allocateDirect(20).order(ByteOrder.LITTLE_ENDIAN);
    private static final ByteBuffer STAGES = ByteBuffer.allocateDirect(FrameChart.STAGES * 8).order(ByteOrder.LITTLE_ENDIAN);
    private static final ByteBuffer PROFILE = ByteBuffer.allocateDirect(FrameChart.CAPACITY).order(ByteOrder.LITTLE_ENDIAN);
    private static Native renderer;
    private static VulkanCommandEncoder encoder;
    private static long generation = -1;
    private static volatile boolean resetRequested;
    private static long lastCompleted;
    private Renderer() {}

    /** 替代原版世界绘制；相机、手持物和 UI 继续使用宿主路径。 */
    public static void render(CameraRenderState camera, RenderTarget target) {
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
        if (renderer == null) {
            var backend = ((DeviceAccess) RenderSystem.getDevice()).primept$backend();
            if (!(backend instanceof VulkanDevice device))
                throw new IllegalStateException("Prime PT 需要 --graphicsBackend VULKAN");
            encoder = device.createCommandEncoder();
            await();
            renderer = new Native(device.instance().vkInstance().address(), device.vkDevice().getPhysicalDevice().address(),
                    device.vkDevice().address(), device.graphicsQueue().vkQueue().address(), device.graphicsQueue().queueFamilyIndex(),
                    Bootstrap.graphicsQueueIndex(), Bootstrap.invocationReorder(device.vkDevice()), Settings.requestedSer());
        }
        long stage = System.nanoTime();
        await();
        STAGES.putLong(0, System.nanoTime() - stage);
        stage = System.nanoTime();
        renderer.ser(Settings.requestedSer());
        if (CAPTURE.select(minecraft.level) || resetRequested) {
            renderer.reset();
            resetRequested = false;
            lastCompleted = 0;
            FrameChart.clear();
        }
        if (generation != Atlas.generation) {
            Atlas.send(renderer); generation = Atlas.generation;
        }
        if (CHANGES.takeOverflow()) renderer.invalidateAll();
        for (int i = 0; i < 256; i++) {
            var change = CHANGES.poll();
            if (change == null) break;
            if (change.world() != minecraft.level) continue;
            if (change.all()) renderer.invalidateAll();
            else renderer.invalidateSection(change.x(), change.y(), change.z());
        }
        STAGES.putLong(8, System.nanoTime() - stage);
        stage = System.nanoTime();
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
        if (!(target.getColorTexture() instanceof VulkanGpuTexture texture)
                || !(target.getColorTextureView() instanceof VulkanGpuTextureView view)
                || texture.getFormat() != GpuFormat.RGBA8_UNORM || (texture.usage() & STORAGE) == 0)
            throw new IllegalStateException("Prime PT 目标必须是支持 storage 的 RGBA8_UNORM");
        stage = System.nanoTime();
        var command = encoder.allocateAndBeginTransientCommandBuffer();
        ENVIRONMENT.putFloat(12, Settings.exposure());
        renderer.draw(command.address(), texture.vkImage(), view.vkImageView(), target.width, target.height,
                CAMERA, ENVIRONMENT, Settings.rr(), Settings.quality(), Settings.samples(), Settings.bounces());
        int status = VK10.vkEndCommandBuffer(command);
        if (status != VK10.VK_SUCCESS) throw new IllegalStateException("vkEndCommandBuffer: " + status);
        STAGES.putLong(88, System.nanoTime() - stage);
        stage = System.nanoTime();
        encoder.execute(command);
        STAGES.putLong(96, System.nanoTime() - stage);
        long completed = System.nanoTime();
        long elapsed = lastCompleted == 0 || minecraft.isPaused() ? 0 : completed - lastCompleted;
        lastCompleted = completed;
        renderer.profile(elapsed, STAGES, PROFILE, FrameChart.columns());
        FrameChart.update(PROFILE);
    }

    private static void await() {
        try (var fence = encoder.createFence()) {
            encoder.submit();
            if (!fence.awaitCompletion(5_000_000_000L))
                throw new IllegalStateException("Prime PT 宿主 GPU 提交未完成；资源保留到进程退出");
        }
    }

    /** 世界替换延迟到下次 render 的完成边界处理。 */
    public static void reset() { resetRequested = true; }
    /** tint 和模型源刷新保留旧覆盖，分步重捕获；不切换世界域。 */
    public static void invalidate(ClientLevel level) { CHANGES.all(level); }
    /** 与宿主天空使用同一个相机 attribute probe 和插值时刻。 */
    public static void environment(ClientLevel level, Camera camera, float partial) {
        EnvironmentPacket.write(level, camera, partial, ENVIRONMENT);
    }
    /** block 变化同时失效相邻 section 的剔除结果。 */
    public static void dirty(ClientLevel level, BlockPos pos) {
        section(level, pos.getX() >> 4, pos.getY() >> 4, pos.getZ() >> 4);
        for (var direction : Direction.values()) {
            var neighbor = pos.relative(direction);
            section(level, neighbor.getX() >> 4, neighbor.getY() >> 4, neighbor.getZ() >> 4);
        }
    }
    /** chunk 加载和卸载统一替换对应 section 源。 */
    public static void column(ClientLevel level, int x, int z) {
        for (int y = level.getMinSectionY(); y <= level.getMaxSectionY(); y++) {
            section(level, x, y, z);
            section(level, x - 1, y, z); section(level, x + 1, y, z);
            section(level, x, y, z - 1); section(level, x, y, z + 1);
        }
    }
    /** 只标记源，GPU 不在世界更新回调中使用。 */
    public static void section(ClientLevel level, int x, int y, int z) { CHANGES.section(level, x, y, z); }

    /** 必须在宿主 VulkanDevice.close 之前调用。 */
    public static void close() {
        if (renderer == null) return;
        await(); renderer.close(); renderer = null; generation = -1; CAPTURE.invalidate(); resetRequested = false;
        lastCompleted = 0; FrameChart.clear();
    }
}
