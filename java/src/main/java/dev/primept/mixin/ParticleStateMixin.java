package dev.primept.mixin;
import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import net.minecraft.client.particle.SingleQuadParticle;
import net.minecraft.client.renderer.state.level.QuadParticleRenderState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(QuadParticleRenderState.class)
abstract class ParticleStateMixin {
    @Inject(method = "add", at = @At("HEAD"), cancellable = true)
    private void primept$instance(SingleQuadParticle.Layer layer, float x, float y, float z, float qx,
            float qy, float qz, float qw, float scale, float u0, float u1, float v0, float v1, int color,
            int light, CallbackInfo ci) {
        if (!Renderer.active())
            return;
        WorldFeatures.billboard(layer, x, y, z, qx, qy, qz, qw, scale, u0, u1, v0, v1, color, light);
        ci.cancel();
    }
}
