package dev.primept.mixin;

import dev.primept.Renderer;
import dev.primept.TerrainSnapshot;
import dev.primept.WorldFeatures;
import net.minecraft.client.Camera;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.LevelRenderer;
import net.minecraft.client.renderer.culling.Frustum;
import net.minecraft.client.renderer.entity.state.EntityRenderState;
import net.minecraft.client.renderer.extract.LevelExtractor;
import net.minecraft.client.renderer.state.level.LevelRenderState;
import net.minecraft.client.renderer.state.level.PlayerRenderState;
import net.minecraft.core.BlockPos;
import net.minecraft.world.entity.Entity;
import org.joml.Matrix4f;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.Redirect;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(LevelExtractor.class)
abstract class ExtractorMixin {
    @Shadow @Final private LevelRenderState levelRenderState;
    @Shadow private ClientLevel level;
    @Shadow @Final private Minecraft minecraft;
    @Shadow @Final private LevelRenderer levelRenderer;
    @Shadow
    protected abstract void extractVisibleEntities(
            Camera camera, Frustum frustum, DeltaTracker delta, LevelRenderState state);

    @Inject(method = "extractEntity", at = @At("RETURN"))
    private void primept$entitySource(
            Entity entity, float partial, CallbackInfoReturnable<EntityRenderState> ci) {
        if (Renderer.active())
            WorldFeatures.link(ci.getReturnValue(), entity);
    }
    @Inject(method = "isEntityVisible", at = @At("HEAD"), cancellable = true)
    private void primept$localPlayer(Entity entity, Frustum frustum, double x, double y, double z,
            float partial, long fade, CallbackInfoReturnable<Boolean> ci) {
        if (Renderer.active() && entity == minecraft.player && minecraft.options.getCameraType().isFirstPerson())
            ci.setReturnValue(false);
    }
    @Redirect(method = "isEntityVisible",
            at = @At(value = "INVOKE",
                    target = "Lnet/minecraft/client/renderer/"
                             + "LevelRenderer;isSectionCompiledAndVisible(Lnet/minecraft/core/"
                             + "BlockPos;J)Z"))
    private boolean
    primept$rayVisibility(LevelRenderer renderer, BlockPos pos, long fade) {
        return Renderer.active() || renderer.isSectionCompiledAndVisible(pos, fade);
    }
    @Shadow
    protected abstract void extractPlayerState(
            Camera camera, DeltaTracker delta, float partial, PlayerRenderState output);

    @Inject(method = "extract", at = @At("HEAD"), cancellable = true)
    private void primept$extract(
            DeltaTracker delta, Camera camera, float partial, CallbackInfo callback) {
        if (!Renderer.active())
            return;
        levelRenderState.reset();
        levelRenderState.worldPartialTicks = partial;
        Renderer.environment(level, camera, partial);
        extractPlayerState(camera, delta, partial, levelRenderState.playerRenderState);
        WorldFeatures.extraction();
        if (level != null) {
            var eye = camera.position();
            levelRenderer.blockEntityRenderDispatcher().prepare(eye);
            levelRenderer.entityRenderDispatcher().prepare(camera, minecraft.crosshairPickEntity);
            float radius = (minecraft.options.getEffectiveRenderDistance() + 1) * 16f;
            var volume = new Frustum(new Matrix4f(),
                    new Matrix4f().ortho(-radius, radius, -radius, radius, -radius, radius));
            volume.prepare(eye.x, eye.y, eye.z);
            extractVisibleEntities(camera, volume, delta, levelRenderState);
            ((TerrainSnapshot.Cache) level.getChunkSource())
                    .primept$blockEntities(level, levelRenderer, camera, partial, levelRenderState);
            minecraft.particleEngine.extract(
                    levelRenderState.particlesRenderState, volume, camera, partial);
            level.getChunkSource().flipUpdateTrackingSets();
        }
        if (Renderer.active())
            callback.cancel();
    }

    @Inject(method = {"blockChanged", "setSectionRangeDirty", "setSectionDirty(IIIZ)V"},
            at = @At("HEAD"), cancellable = true)
    private void primept$skipRasterDirty(CallbackInfo callback) {
        // 方块、整列和 biome 的实际写入已各自通知 Prime。光栅失效无须重捕获。
        if (Renderer.active())
            callback.cancel();
    }

    @Inject(method = "allChanged", at = @At("HEAD"), cancellable = true)
    private void primept$invalidate(CallbackInfo callback) {
        if (!Renderer.active())
            return;
        if (level != null)
            level.clearTintCaches();
        if (Renderer.active())
            callback.cancel();
    }

    @Inject(method = "queueTransientBlock", at = @At("HEAD"), cancellable = true)
    private void primept$skipTransient(CallbackInfo callback) {
        if (Renderer.active())
            callback.cancel();
    }
}
