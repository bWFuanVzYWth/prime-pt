package dev.primept.mixin;

import com.mojang.blaze3d.pipeline.MainTarget;
import dev.primept.Renderer;
import dev.primept.Bootstrap;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.ModifyArg;

@Mixin(MainTarget.class)
abstract class MainTargetMixin {
    @ModifyArg(method = "allocateColorAttachment", at = @At(value = "INVOKE", target = "Lcom/mojang/renderpearl/api/device/GpuDevice;createTexture(Ljava/util/function/Supplier;ILcom/mojang/renderpearl/api/GpuFormat;IIII)Lcom/mojang/renderpearl/api/textures/GpuTexture;"), index = 1)
    private int primept$usage(int usage) { return Bootstrap.rayTracingEnabled() ? usage | Renderer.STORAGE : usage; }
}
