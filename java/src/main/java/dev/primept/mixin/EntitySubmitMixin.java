package dev.primept.mixin;
import com.mojang.blaze3d.vertex.PoseStack;
import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.entity.EntityRenderDispatcher;
import net.minecraft.client.renderer.entity.state.EntityRenderState;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(EntityRenderDispatcher.class)
abstract class EntitySubmitMixin {
    @Inject(method = "submit", at = @At("HEAD"))
    private void primept$source(EntityRenderState state, CameraRenderState camera, double x, double y,
            double z, PoseStack poses, SubmitNodeCollector collector, CallbackInfo ci) {
        WorldFeatures.submitting(state);
    }
    @Inject(method = "submit", at = @At("RETURN"))
    private void primept$end(CallbackInfo ci) {
        WorldFeatures.submitted();
    }
}
