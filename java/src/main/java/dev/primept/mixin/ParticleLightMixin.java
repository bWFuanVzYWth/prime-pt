package dev.primept.mixin;

import dev.primept.WorldFeatures;
import net.minecraft.client.particle.Particle;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(Particle.class)
abstract class ParticleLightMixin {
    // 路径追踪负责场景照明；原版子类额外叠加的亮度才是粒子自身发光。
    @Inject(method = "getLightCoords", at = @At("HEAD"), cancellable = true)
    private void primept$baseLight(float partial, CallbackInfoReturnable<Integer> result) {
        if (WorldFeatures.extractingParticle())
            result.setReturnValue(0);
    }
}
