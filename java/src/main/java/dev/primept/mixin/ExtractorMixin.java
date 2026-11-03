package dev.primept.mixin;

import dev.primept.Renderer;
import net.minecraft.client.Camera;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.extract.LevelExtractor;
import net.minecraft.client.renderer.state.level.LevelRenderState;
import net.minecraft.client.renderer.state.level.PlayerRenderState;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(LevelExtractor.class)
abstract class ExtractorMixin {
    @Shadow @Final private LevelRenderState levelRenderState;
    @Shadow private ClientLevel level;
    @Shadow protected abstract void extractPlayerState(Camera camera, DeltaTracker delta, float partial, PlayerRenderState output);

    @Inject(method = "extract", at = @At("HEAD"), cancellable = true)
    private void primept$extract(DeltaTracker delta, Camera camera, float partial, CallbackInfo callback) {
        levelRenderState.reset();
        levelRenderState.worldPartialTicks = partial;
        Renderer.environment(level, camera, partial);
        extractPlayerState(camera, delta, partial, levelRenderState.playerRenderState);
        if (level != null) level.getChunkSource().flipUpdateTrackingSets();
        callback.cancel();
    }

    @Inject(method = "setSectionDirty(IIIZ)V", at = @At("HEAD"), cancellable = true)
    private void primept$dirty(int x, int y, int z, boolean playerChanged, CallbackInfo callback) {
        Renderer.section(level, x, y, z);
        callback.cancel();
    }

    @Inject(method = "allChanged", at = @At("HEAD"), cancellable = true)
    private void primept$invalidate(CallbackInfo callback) {
        if (level != null) level.clearTintCaches();
        Renderer.invalidate(level);
        callback.cancel();
    }

    @Inject(method = "queueTransientBlock", at = @At("HEAD"), cancellable = true)
    private void primept$skipTransient(CallbackInfo callback) { callback.cancel(); }
}
