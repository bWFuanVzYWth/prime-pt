package dev.primept.mixin;

import com.llamalad7.mixinextras.injector.ModifyExpressionValue;
import dev.primept.WorldFeatures;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(targets = {"net.minecraft.client.particle.ItemPickupParticleGroup$State",
               "net.minecraft.client.particle.ElderGuardianParticleGroup$State"})
abstract class ModelParticleSubmitMixin {
    @ModifyExpressionValue(method = "submit",
            at = @At(value = "INVOKE", target = "Ljava/util/Iterator;next()Ljava/lang/Object;"))
    private Object
    primept$source(Object instance) {
        if (WorldFeatures.active())
            WorldFeatures.particleModel(instance);
        return instance;
    }
    @Inject(method = "submit", at = @At("RETURN"))
    private void primept$end(CallbackInfo ci) {
        if (WorldFeatures.active())
            WorldFeatures.endParticleModels();
    }
}
