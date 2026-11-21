package dev.primept.mixin;

import com.llamalad7.mixinextras.injector.wrapmethod.WrapMethod;
import com.llamalad7.mixinextras.injector.wrapoperation.Operation;
import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.item.ItemModelResolver;
import net.minecraft.client.renderer.item.ItemStackRenderState;
import net.minecraft.world.entity.ItemOwner;
import net.minecraft.world.item.ItemDisplayContext;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;
import org.spongepowered.asm.mixin.Mixin;

@Mixin(ItemModelResolver.class)
abstract class ItemModelSourceMixin {
    @WrapMethod(method = "appendItemLayers")
    private void primept$stack(ItemStackRenderState state, ItemStack stack, ItemDisplayContext display,
            Level level, ItemOwner owner, int seed, Operation<Void> append) {
        var layers = (WorldFeatures.ItemLayers) state;
        int previous = layers.primept$block(WorldFeatures.blockState(stack));
        try {
            append.call(state, stack, display, level, owner, seed);
        } finally {
            layers.primept$block(previous);
        }
    }
}
