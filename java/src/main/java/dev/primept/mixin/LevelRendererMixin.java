package dev.primept.mixin;

import dev.primept.Renderer;
import net.minecraft.client.Camera;
import net.minecraft.client.Options;
import net.minecraft.client.color.block.BlockColors;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.LevelRenderer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(LevelRenderer.class)
abstract class LevelRendererMixin {
    @Inject(method = "invalidateCompiledGeometry", at = @At("RETURN"))
    private void primept$restore(ClientLevel level, Options options, Camera camera, BlockColors colors,
            CallbackInfo callback) {
        Renderer.restoreVanilla(level, ((LevelRenderer) (Object) this).sectionOcclusionGraph());
    }
}
