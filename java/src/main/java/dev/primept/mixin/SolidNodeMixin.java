package dev.primept.mixin;
import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.feature.phase.SimpleFeatureRenderPhase;
import net.minecraft.client.renderer.feature.submit.SubmitNode;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(SimpleFeatureRenderPhase.class)
abstract class SolidNodeMixin {
    @Inject(method = "submit(Lnet/minecraft/client/renderer/feature/submit/SubmitNode;)V",
            at = @At("HEAD"))
    private void
    primept$source(SubmitNode node, CallbackInfo ci) {
        WorldFeatures.tag(node);
    }
}
