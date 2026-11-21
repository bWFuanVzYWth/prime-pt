package dev.primept.mixin;
import com.mojang.blaze3d.vertex.PoseStack;
import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.blockentity.BlockEntityRenderDispatcher;
import net.minecraft.client.renderer.blockentity.state.BlockEntityRenderState;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(BlockEntityRenderDispatcher.class)
abstract class BlockSubmitMixin {
    @Inject(method = "submit", at = @At("HEAD"))
    private void primept$source(BlockEntityRenderState state, PoseStack poses,
            SubmitNodeCollector collector, CameraRenderState camera, CallbackInfo ci) {
        WorldFeatures.submitting(state);
    }
    @Inject(method = "submit", at = @At("RETURN"))
    private void primept$end(CallbackInfo ci) {
        WorldFeatures.submitted();
    }
}
