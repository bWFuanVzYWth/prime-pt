package dev.primept.mixin;

import dev.primept.Renderer;
import com.mojang.blaze3d.resource.GraphicsResourceAllocator;
import com.mojang.renderpearl.api.buffers.GpuBufferSlice;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.GameRenderer;
import net.minecraft.client.renderer.LevelRenderer;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import org.joml.Vector4f;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.Redirect;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(GameRenderer.class)
abstract class GameRendererMixin {
    @Redirect(method = "renderLevel", at = @At(value = "INVOKE", target = "Lnet/minecraft/client/renderer/LevelRenderer;render(Lcom/mojang/blaze3d/resource/GraphicsResourceAllocator;ZLnet/minecraft/client/renderer/state/level/CameraRenderState;Lcom/mojang/renderpearl/api/buffers/GpuBufferSlice;Lorg/joml/Vector4f;ZZ)V"))
    private void primept$render(LevelRenderer vanilla, GraphicsResourceAllocator allocator, boolean outline,
            CameraRenderState camera, GpuBufferSlice fog, Vector4f fogColor, boolean sky, boolean effects) {
        var renderer = (GameRenderer) (Object) this;
        Renderer.render(camera, renderer.mainRenderTarget());
    }

    @Inject(method = "setLevel", at = @At("HEAD"))
    private void primept$reset(ClientLevel level, CallbackInfo callback) { Renderer.reset(); }

    @Inject(method = "close", at = @At("HEAD"))
    private void primept$close(CallbackInfo callback) { Renderer.close(); }
}
