package dev.primept.mixin;

import com.llamalad7.mixinextras.injector.wrapmethod.WrapMethod;
import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import com.mojang.blaze3d.vertex.PoseStack;
import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.item.ItemStackRenderState;
import net.minecraft.client.resources.model.geometry.ItemQuads;
import org.spongepowered.asm.mixin.*;

@Mixin(ItemStackRenderState.LayerRenderState.class)
abstract class ItemLayerMixin implements WorldFeatures.ItemLayers {
    @Shadow private ItemQuads quads;
    @Unique private int primept$source = -1;
    public int primept$block(int id) {
        int old = primept$source;
        primept$source = id;
        return old;
    }
    public boolean primept$emits() { return WorldFeatures.emits(primept$source, quads); }
    @WrapMethod(method = "submit")
    private void primept$item(PoseStack pose, SubmitNodeCollector collector, int light, int overlay,
            int outline, Operation<Void> submit) {
        if (!WorldFeatures.active()) {
            if (Renderer.active() && WorldFeatures.capturedLayer(this)) return;
            submit.call(pose, collector, light, overlay, outline);
            return;
        }
        boolean hand = WorldFeatures.collectingHand();
        if (hand && !primept$emits()) return;
        int before = WorldFeatures.spanCount();
        int previous = WorldFeatures.itemBlock(primept$source);
        try {
            submit.call(pose, collector, light, overlay, outline);
            if (hand) WorldFeatures.captureLayer(this, before);
        } finally {
            WorldFeatures.itemBlock(previous);
        }
    }
}
