package dev.primept.mixin;

import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import dev.primept.CameraPacket;
import com.mojang.blaze3d.vertex.PoseStack;
import net.minecraft.client.Minecraft;
import net.minecraft.client.renderer.FirstPersonHandsAndItemsRenderer;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.state.GameRenderState;
import net.minecraft.client.renderer.state.level.PlayerRenderState;
import net.minecraft.world.level.GameType;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.Final;
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
abstract class GameRendererMixin implements WorldFeatures.HandRenderer {
    @Shadow @Final private FirstPersonHandsAndItemsRenderer firstPersonHandsAndItemsRenderer;
    @Shadow @Final private GameRenderState gameRenderState;
    @Shadow private void bobHurt(CameraRenderState camera, PoseStack poses) {}
    @Shadow private void bobView(CameraRenderState camera, PoseStack poses) {}

    public void primept$hands(CameraRenderState camera, PlayerRenderState player, SubmitNodeCollector collector) {
        var state = player.firstPersonHandsAndItems;
        var options = gameRenderState.optionsRenderState;
        if (camera.isPanoramicMode || !player.hasPlayer || state == null || !options.cameraType.isFirstPerson()
                || camera.entityRenderState.isSleeping || gameRenderState.guiRenderState.isHudHidden
                || Minecraft.getInstance().gameMode.getPlayerMode() == GameType.SPECTATOR
                || state.isScoping
                || !(((WorldFeatures.ItemLayers) state.mainHandRenderState).primept$emits()
                    || ((WorldFeatures.ItemLayers) state.offHandRenderState).primept$emits())) return;
        var poses = new PoseStack();
        var window = gameRenderState.windowRenderState;
        poses.mulPose(CameraPacket.handTransform(camera, window.width, window.height));
        bobHurt(camera, poses);
        if (options.bobView) bobView(camera, poses);
        WorldFeatures.beginHands();
        try {
            firstPersonHandsAndItemsRenderer.submitHandsWithItems(camera.cameraEntityPartialTicks,
                    poses, collector, player, state);
        } finally {
            WorldFeatures.submittedHands();
        }
    }

    @Inject(method = "extract", at = @At("HEAD"))
    private void primept$beginFrame(CallbackInfo callback) { Renderer.beginFrame(); }

    @Inject(method = "renderItemInHand", at = @At("HEAD"), cancellable = true)
    private void primept$handPass(CallbackInfo callback) {
        if (Renderer.active() && Renderer.frozen()) callback.cancel();
    }

    @Redirect(method = "renderLevel", at = @At(value = "INVOKE", target = "Lnet/minecraft/client/renderer/LevelRenderer;render(Lcom/mojang/blaze3d/resource/GraphicsResourceAllocator;ZLnet/minecraft/client/renderer/state/level/CameraRenderState;Lcom/mojang/renderpearl/api/buffers/GpuBufferSlice;Lorg/joml/Vector4f;ZZ)V"))
    private void primept$render(LevelRenderer vanilla, GraphicsResourceAllocator allocator, boolean outline,
            CameraRenderState camera, GpuBufferSlice fog, Vector4f fogColor, boolean sky, boolean effects) {
        var renderer = (GameRenderer) (Object) this;
        if (Renderer.active()) Renderer.render(camera, renderer.mainRenderTarget());
        else vanilla.render(allocator, outline, camera, fog, fogColor, sky, effects);
    }

    @Inject(method = "setLevel", at = @At("HEAD"))
    private void primept$reset(ClientLevel level, CallbackInfo callback) { Renderer.reset(); }

    @Inject(method = "close", at = @At("HEAD"))
    private void primept$close(CallbackInfo callback) { Renderer.close(); }
}
