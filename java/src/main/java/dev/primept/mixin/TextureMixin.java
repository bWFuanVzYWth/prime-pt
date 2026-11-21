package dev.primept.mixin;

import com.mojang.renderpearl.backend.vulkan.VulkanGpuTexture;
import dev.primept.Renderer;
import dev.primept.WorldFeatures;
import org.lwjgl.vulkan.VK10;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Unique;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.ModifyArg;

@Mixin(VulkanGpuTexture.class)
abstract class TextureMixin implements WorldFeatures.TextureVersion {
    @Unique private long primept$revision;
    public long primept$revision() {
        return primept$revision;
    }
    public void primept$written() {
        primept$revision++;
    }
    @ModifyArg(method = "<init>",
            at = @At(value = "INVOKE",
                    target = "Lorg/lwjgl/vulkan/VkImageCreateInfo;usage(I)Lorg/lwjgl/vulkan/"
                             + "VkImageCreateInfo;"),
            index = 0)
    private int
    primept$storage(int usage) {
        return (((VulkanGpuTexture) (Object) this).usage() & Renderer.STORAGE) != 0
                ? usage | VK10.VK_IMAGE_USAGE_STORAGE_BIT
                : usage;
    }
}
