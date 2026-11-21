package dev.primept.mixin;

import com.mojang.blaze3d.vertex.PoseStack;
import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import net.minecraft.client.Camera;
import net.minecraft.client.Options;
import net.minecraft.client.color.block.BlockColors;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.LevelRenderer;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.SubmitNodeStorage;
import net.minecraft.client.renderer.feature.FeatureRenderDispatcher;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import net.minecraft.client.renderer.state.level.LevelRenderState;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(LevelRenderer.class)
abstract class LevelRendererMixin implements WorldFeatures.SourceRenderer {
    @Shadow @Final private SubmitNodeStorage submitNodeStorage;
    @Shadow @Final private LevelRenderState levelRenderState;
    @Shadow @Final private FeatureRenderDispatcher featureRenderDispatcher;
    @Shadow
    protected abstract void submitEntities(
            PoseStack pose, LevelRenderState state, SubmitNodeCollector collector);
    @Shadow
    protected abstract void submitBlockEntities(
            PoseStack pose, LevelRenderState state, SubmitNodeCollector collector);
    public void primept$features(CameraRenderState camera) {
        var poses = new PoseStack();
        WorldFeatures.begin();
        try {
            levelRenderState.cameraRenderState = camera;
            submitEntities(poses, levelRenderState, submitNodeStorage);
            submitBlockEntities(poses, levelRenderState, submitNodeStorage);
            levelRenderState.particlesRenderState.submit(submitNodeStorage, camera);
            featureRenderDispatcher.prepareFrame(submitNodeStorage).close();
        } finally {
            WorldFeatures.end();
        }
    }
    @Inject(method = "invalidateCompiledGeometry", at = @At("RETURN"))
    private void primept$restore(ClientLevel level, Options options, Camera camera, BlockColors colors,
            CallbackInfo callback) {
        Renderer.restoreVanilla(level, ((LevelRenderer) (Object) this).sectionOcclusionGraph());
    }
}
