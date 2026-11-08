package dev.primept.mixin;

import com.mojang.renderpearl.backend.vulkan.VulkanGpuSurface;
import dev.primept.StreamlineBootstrap;
import dev.primept.WindowDiagnostic;
import java.nio.LongBuffer;
import org.lwjgl.sdl.*;
import org.lwjgl.system.*;
import org.lwjgl.system.windows.User32;
import org.lwjgl.vulkan.*;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Redirect;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** 窗口 surface 与宿主实例使用同一个 interposed dispatch。 */
@Mixin(VulkanGpuSurface.class)
abstract class StreamlineSurfaceMixin {
    @Inject(method = "configure", at = @At("HEAD"))
    private void primept$configureBegin(CallbackInfo callback) { WindowDiagnostic.mark("swapchain.configure BEGIN"); }
    @Inject(method = "configure", at = @At("RETURN"))
    private void primept$configureEnd(CallbackInfo callback) { WindowDiagnostic.mark("swapchain.configure END"); }
    @Inject(method = "destroySwapchain", at = @At("HEAD"))
    private void primept$destroyBegin(CallbackInfo callback) { WindowDiagnostic.mark("swapchain.destroy BEGIN"); }
    @Inject(method = "destroySwapchain", at = @At("RETURN"))
    private void primept$destroyEnd(CallbackInfo callback) { WindowDiagnostic.mark("swapchain.destroy END"); }
    @Redirect(method = "<init>", at = @At(value = "INVOKE", target = "Lorg/lwjgl/sdl/SDLVulkan;SDL_Vulkan_CreateSurface(JLorg/lwjgl/vulkan/VkInstance;Lorg/lwjgl/vulkan/VkAllocationCallbacks;Ljava/nio/LongBuffer;)Z"))
    private boolean primept$surface(long window, VkInstance instance, VkAllocationCallbacks allocator, LongBuffer output) {
        if (!StreamlineBootstrap.installed())
            return SDLVulkan.SDL_Vulkan_CreateSurface(window, instance, allocator, output);
        int properties = SDLVideo.SDL_GetWindowProperties(window);
        long hwnd = SDLProperties.SDL_GetPointerProperty(properties, SDLVideo.SDL_PROP_WINDOW_WIN32_HWND_POINTER, 0);
        long hinstance = hwnd == 0 ? 0 : User32.GetWindowLongPtr(hwnd, User32.GWL_HINSTANCE);
        long function = VK10.vkGetInstanceProcAddr(instance, "vkCreateWin32SurfaceKHR");
        if (hwnd == 0 || hinstance == 0 || function == 0)
            throw new IllegalStateException("缺少 interposed Win32 surface 入口");
        try (var stack = MemoryStack.stackPush()) {
            var create = VkWin32SurfaceCreateInfoKHR.calloc(stack).sType$Default().hwnd(hwnd).hinstance(hinstance);
            int result = JNI.callPPPPI(instance.address(), create.address(), MemoryUtil.memAddressSafe(allocator),
                    MemoryUtil.memAddress(output), function);
            if (result != VK10.VK_SUCCESS) throw new IllegalStateException("interposed vkCreateWin32SurfaceKHR: " + result);
            return true;
        }
    }
}
