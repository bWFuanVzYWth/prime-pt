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
        if (!Renderer.active()) return;
        levelRenderState.reset();
        levelRenderState.worldPartialTicks = partial;
        Renderer.environment(level, camera, partial);
        extractPlayerState(camera, delta, partial, levelRenderState.playerRenderState);
        if (level != null) level.getChunkSource().flipUpdateTrackingSets();
        if (Renderer.active()) callback.cancel();
    }

    @Inject(method = {"blockChanged", "setSectionRangeDirty", "setSectionDirty(IIIZ)V"},
            at = @At("HEAD"), cancellable = true)
    private void primept$skipRasterDirty(CallbackInfo callback) {
        // 方块、整列和 biome 的实际写入已各自通知 Prime。光栅失效无须重捕获。
        if (Renderer.active()) callback.cancel();
    }

    @Inject(method = "allChanged", at = @At("HEAD"), cancellable = true)
    private void primept$invalidate(CallbackInfo callback) {
        if (!Renderer.active()) return;
        if (level != null) level.clearTintCaches();
        if (Renderer.active()) callback.cancel();
    }

    @Inject(method = "queueTransientBlock", at = @At("HEAD"), cancellable = true)
    private void primept$skipTransient(CallbackInfo callback) { if (Renderer.active()) callback.cancel(); }
}
