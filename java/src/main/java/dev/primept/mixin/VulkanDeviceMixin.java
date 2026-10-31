package dev.primept.mixin;

import com.mojang.renderpearl.backend.vulkan.VulkanDevice;
import dev.primept.Renderer;
import dev.primept.StreamlineBootstrap;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** 在设备真正释放前完成 native 租约，覆盖提前退出和渲染器关闭顺序变化。 */
@Mixin(VulkanDevice.class)
abstract class VulkanDeviceMixin {
    @Inject(method = "close", at = @At("HEAD"))
    private void primept$close(CallbackInfo callback) {
        Renderer.close();
        StreamlineBootstrap.shutdown();
    }
}
