package dev.primept.mixin;

import com.llamalad7.mixinextras.injector.ModifyExpressionValue;
import com.llamalad7.mixinextras.sugar.Local;
import com.mojang.renderpearl.backend.vulkan.VulkanBackend;
import com.mojang.renderpearl.backend.vulkan.VulkanPhysicalDevice;
import com.mojang.renderpearl.backend.vulkan.init.FeatureSet;
import dev.primept.Bootstrap;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;
import org.lwjgl.vulkan.VkDevice;

@Mixin(VulkanBackend.class)
abstract class BackendMixin {
    @ModifyExpressionValue(method = "createDevice(Lcom/mojang/renderpearl/api/device/GpuDebugOptions;)Lcom/mojang/renderpearl/api/device/GpuDevice;",
            at = @At(value = "NEW", target = "(Ljava/lang/String;Ljava/util/Collection;)Lcom/mojang/renderpearl/backend/vulkan/init/FeatureSet;"))
    private FeatureSet primept$features(FeatureSet original, @Local VulkanPhysicalDevice physical) {
        return Bootstrap.enable(original, physical);
    }

    @Inject(method = "createDevice(Lcom/mojang/renderpearl/backend/vulkan/init/FeatureSet;Lcom/mojang/renderpearl/backend/vulkan/VulkanPhysicalDevice;)Lorg/lwjgl/vulkan/VkDevice;", at = @At("RETURN"))
    private static void primept$created(FeatureSet enabled, VulkanPhysicalDevice physical, CallbackInfoReturnable<VkDevice> callback) {
        Bootstrap.created(enabled, callback.getReturnValue());
    }
}
