package dev.primept.mixin;

import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import net.minecraft.client.Camera;
import net.minecraft.client.particle.Particle;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Unique;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Coerce;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(targets = {"net.minecraft.client.particle.ItemPickupParticleGroup$ParticleInstance",
               "net.minecraft.client.particle.ElderGuardianParticleGroup$"
               + "ElderGuardianParticleRenderState"})
abstract class ModelParticleSourceMixin implements WorldFeatures.SourceState {
    @Unique private long primept$source;
    public long primept$sourceId() {
        return primept$source;
    }
    public void primept$sourceId(long id) {
        primept$source = id;
    }
    @Inject(method = "fromParticle", at = @At("RETURN"))
    private static void primept$link(
            @Coerce Particle particle, Camera camera, float partial, CallbackInfoReturnable<?> result) {
        if (Renderer.active())
            WorldFeatures.link(result.getReturnValue(), particle);
    }
}
