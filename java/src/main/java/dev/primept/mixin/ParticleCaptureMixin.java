package dev.primept.mixin;

import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import com.llamalad7.mixinextras.injector.wrapoperation.WrapOperation;
import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import net.minecraft.client.Camera;
import net.minecraft.client.particle.QuadParticleGroup;
import net.minecraft.client.particle.SingleQuadParticle;
import net.minecraft.client.renderer.state.level.QuadParticleRenderState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;

@Mixin(QuadParticleGroup.class)
abstract class ParticleCaptureMixin {
    @WrapOperation(method = "extractRenderState",
            at = @At(value = "INVOKE",
                    target = "Lnet/minecraft/client/particle/SingleQuadParticle;extract(Lnet/minecraft/"
                             + "client/renderer/state/level/QuadParticleRenderState;Lnet/minecraft/"
                             + "client/Camera;F)V"))
    private void
    primept$extract(SingleQuadParticle particle, QuadParticleRenderState state, Camera camera,
            float partial, Operation<Void> extract) {
        if (!Renderer.active()) {
            extract.call(particle, state, camera, partial);
            return;
        }
        WorldFeatures.particleOwner(particle);
        try {
            extract.call(particle, state, camera, partial);
        } finally {
            WorldFeatures.particleOwner(null);
        }
    }
}
