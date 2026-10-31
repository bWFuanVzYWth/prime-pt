package dev.primept.mixin;

import com.mojang.renderpearl.backend.vulkan.VulkanGpuSurface;
import dev.primept.StreamlineBootstrap;
import java.nio.LongBuffer;
import org.lwjgl.sdl.*;
import org.lwjgl.system.*;
import org.lwjgl.system.windows.User32;
import org.lwjgl.vulkan.*;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Redirect;

/** 窗口 surface 与宿主实例使用同一个 interposed dispatch。 */
@Mixin(VulkanGpuSurface.class)
abstract class StreamlineSurfaceMixin {
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
