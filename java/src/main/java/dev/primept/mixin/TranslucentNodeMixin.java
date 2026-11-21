package dev.primept.mixin;
import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.feature.phase.TranslucentFeatureRenderPhase;
import net.minecraft.client.renderer.feature.submit.TranslucentSubmit;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(TranslucentFeatureRenderPhase.class)
abstract class TranslucentNodeMixin {
    @Inject(method = "submit(Lnet/minecraft/client/renderer/feature/submit/TranslucentSubmit;)V",
            at = @At("HEAD"))
    private void
    primept$source(TranslucentSubmit node, CallbackInfo ci) {
        WorldFeatures.tag(node);
    }
}
