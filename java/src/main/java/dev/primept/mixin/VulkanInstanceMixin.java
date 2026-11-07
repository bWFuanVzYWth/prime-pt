package dev.primept.mixin;

import com.mojang.renderpearl.backend.vulkan.VulkanInstance;
import dev.primept.Bootstrap;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.ModifyArg;

/** 能力足够时请求引擎版本；不足时允许原版设备初始化并显示兼容性通知。 */
@Mixin(VulkanInstance.class)
abstract class VulkanInstanceMixin {
    @ModifyArg(method = "<init>", at = @At(value = "INVOKE",
            target = "Lorg/lwjgl/vulkan/VkApplicationInfo;apiVersion(I)Lorg/lwjgl/vulkan/VkApplicationInfo;"), index = 0)
    private int primept$api(int original) { return Bootstrap.instanceApi(original); }
}
