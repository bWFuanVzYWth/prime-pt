package dev.primept.mixin;

import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.item.ItemStackRenderState;
import org.spongepowered.asm.mixin.*;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(ItemStackRenderState.class)
abstract class ItemStateMixin implements WorldFeatures.ItemLayers {
    @Shadow private int activeLayerCount;
    @Shadow private ItemStackRenderState.LayerRenderState[] layers;
    @Unique private int primept$source = -1;
    public int primept$block(int id) {
        int old = primept$source;
        primept$source = id;
        return old;
    }
    public boolean primept$emits() {
        for (int i = 0; i < activeLayerCount; i++)
            if (((WorldFeatures.ItemLayers) layers[i]).primept$emits()) return true;
        return false;
    }
    @Inject(method = "newLayer", at = @At("RETURN"))
    private void primept$setSource(CallbackInfoReturnable<ItemStackRenderState.LayerRenderState> ci) {
        ((WorldFeatures.ItemLayers) ci.getReturnValue()).primept$block(primept$source);
    }
}
