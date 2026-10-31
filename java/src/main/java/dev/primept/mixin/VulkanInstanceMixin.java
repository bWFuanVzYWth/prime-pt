package dev.primept.mixin;

import com.mojang.renderpearl.backend.vulkan.VulkanInstance;
import dev.primept.Bootstrap;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.ModifyArg;

/** 所有宿主实例实际请求引擎的 Vulkan 1.4 基线，包括设备探测实例。 */
@Mixin(VulkanInstance.class)
abstract class VulkanInstanceMixin {
    @ModifyArg(method = "<init>", at = @At(value = "INVOKE",
            target = "Lorg/lwjgl/vulkan/VkApplicationInfo;apiVersion(I)Lorg/lwjgl/vulkan/VkApplicationInfo;"), index = 0)
    private int primept$api(int original) { return Bootstrap.instanceApi(); }
}
