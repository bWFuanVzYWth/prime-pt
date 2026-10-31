package dev.primept.mixin;

import dev.primept.Atlas;
import net.minecraft.client.renderer.texture.SpriteLoader;
import net.minecraft.client.renderer.texture.TextureAtlas;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(TextureAtlas.class)
abstract class AtlasMixin {
    @Inject(method = "upload", at = @At("RETURN"))
    private void primept$atlas(SpriteLoader.Preparations source, CallbackInfo callback) {
        Atlas.capture((TextureAtlas) (Object) this, source);
    }
}
