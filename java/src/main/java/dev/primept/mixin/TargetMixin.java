package dev.primept.mixin;

import com.mojang.blaze3d.pipeline.MainTarget;
import com.mojang.blaze3d.pipeline.RenderTarget;
import dev.primept.Renderer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.ModifyArg;

@Mixin(RenderTarget.class)
abstract class TargetMixin {
    @ModifyArg(method = "createBuffers", at = @At(value = "INVOKE", target = "Lcom/mojang/renderpearl/api/device/GpuDevice;createTexture(Ljava/util/function/Supplier;ILcom/mojang/renderpearl/api/GpuFormat;IIII)Lcom/mojang/renderpearl/api/textures/GpuTexture;", ordinal = 1), index = 1)
    private int primept$resizeUsage(int usage) {
        return (Object) this instanceof MainTarget ? usage | Renderer.STORAGE : usage;
    }
}
