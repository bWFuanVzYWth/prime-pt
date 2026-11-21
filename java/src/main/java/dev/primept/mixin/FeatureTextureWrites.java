package dev.primept.mixin;

import com.mojang.renderpearl.api.buffers.GpuBufferSlice;
import com.mojang.renderpearl.api.textures.GpuTexture;
import com.mojang.renderpearl.backend.vulkan.VulkanCommandEncoder;
import dev.primept.WorldFeatures;
import java.nio.ByteBuffer;
import org.joml.Vector4fc;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(VulkanCommandEncoder.class)
abstract class FeatureTextureWrites {
    @Inject(method = "writeToTexture", at = @At("RETURN"))
    private void primept$write(GpuTexture tex, ByteBuffer src, int mip, int layer, int x, int y, int w,
            int h, CallbackInfo ci) {
        ((WorldFeatures.TextureVersion) tex).primept$written();
    }
    @Inject(method = "copyBufferToTexture", at = @At("RETURN"))
    private void primept$upload(GpuBufferSlice source, int offset, int row, int rows, int depth,
            GpuTexture dst, int mip, int layer, int x, int y, int w, int h, CallbackInfo ci) {
        ((WorldFeatures.TextureVersion) dst).primept$written();
    }
    @Inject(method = "copyTextureToTexture", at = @At("RETURN"))
    private void primept$copy(GpuTexture src, GpuTexture dst, int mip, int x, int y, int u, int v, int w,
            int h, CallbackInfo ci) {
        ((WorldFeatures.TextureVersion) dst).primept$written();
    }
    @Inject(method = "clearColorTexture", at = @At("RETURN"))
    private void primept$clear(GpuTexture tex, Vector4fc color, CallbackInfo ci) {
        ((WorldFeatures.TextureVersion) tex).primept$written();
    }
}
