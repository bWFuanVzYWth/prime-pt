package dev.primept;

import org.lwjgl.sdl.SDLInit;
import org.lwjgl.sdl.SDLVulkan;
import org.lwjgl.vulkan.*;
import org.lwjgl.system.MemoryStack;
import com.mojang.renderpearl.backend.vulkan.VulkanBackend;
import com.mojang.renderpearl.backend.vulkan.VulkanPhysicalDevice;
import com.mojang.renderpearl.backend.vulkan.init.FeatureSet;
import java.util.Set;

/** 无窗口验证 early interposer、Vulkan 1.4 与宿主真实 feature 创建链。 */
final class StreamlineFixture {
    public static void main(String[] arguments) throws Exception {
        StreamlineBootstrap.install();
        require(StreamlineBootstrap.installed(), StreamlineBootstrap.status());
        StreamlineBootstrap.configureSdl();
        VK.create();
        require(SDLInit.SDL_Init(SDLInit.SDL_INIT_VIDEO), "SDL video 初始化失败");
        try {
            require(SDLVulkan.SDL_Vulkan_LoadLibrary((CharSequence) null), "SDL Vulkan loader 初始化失败");
            try {
                long lwjgl = VK.getFunctionProvider().getFunctionAddress("vkGetInstanceProcAddr");
                long sdl = SDLVulkan.SDL_Vulkan_GetVkGetInstanceProcAddr();
                require(lwjgl != 0 && lwjgl == sdl, "SDL 与 LWJGL 的 Vulkan dispatch 不一致");
                device();
            } finally {
                SDLVulkan.SDL_Vulkan_UnloadLibrary();
            }
            StreamlineBootstrap.shutdown();
            System.out.println("Streamline early bootstrap / SDL-LWJGL / Vulkan 1.4 device fixture passed");
        } finally {
            SDLInit.SDL_Quit();
            VK.destroy();
        }
    }

    private static void device() throws Exception {
        try (var stack = MemoryStack.stackPush()) {
            var application = VkApplicationInfo.calloc(stack).sType$Default().apiVersion(Bootstrap.instanceApi());
            var extensions = SDLVulkan.SDL_Vulkan_GetInstanceExtensions();
            require(extensions != null, "SDL Vulkan instance 扩展不可用");
            var info = VkInstanceCreateInfo.calloc(stack).sType$Default().pApplicationInfo(application)
                    .ppEnabledExtensionNames(extensions);
            var pointer = stack.mallocPointer(1);
            require(VK10.vkCreateInstance(info, null, pointer) == VK10.VK_SUCCESS, "Vulkan 1.4 instance 创建失败");
            var instance = new VkInstance(pointer.get(0), info);
            try {
                require(instance.getCapabilities().Vulkan14, "实例实际未启用 Vulkan 1.4");
                var count = stack.mallocInt(1);
                require(VK10.vkEnumeratePhysicalDevices(instance, count, null) == VK10.VK_SUCCESS, "设备枚举失败");
                var devices = stack.mallocPointer(count.get(0));
                require(VK10.vkEnumeratePhysicalDevices(instance, count, devices) == VK10.VK_SUCCESS && count.get(0) > 0, "缺少物理设备");
                try (var physical = new VulkanPhysicalDevice(new VkPhysicalDevice(devices.get(0), instance))) {
                    var enabled = Bootstrap.enable(new FeatureSet("Prime fixture", Set.of(), Set.of()), physical);
                    var create = VulkanBackend.class.getDeclaredMethod("createDevice", FeatureSet.class, VulkanPhysicalDevice.class);
                    create.setAccessible(true);
                    var device = (VkDevice) create.invoke(null, enabled, physical);
                    try {
                        Bootstrap.created(enabled, device);
                        require(device.getCapabilities().Vulkan14, "逻辑设备实际未使用 Vulkan 1.4");
                        require(device.getCapabilities().VK_KHR_device_address_commands, "deviceAddressCommands 扩展未启用");
                        require(StreamlineBootstrap.available(), StreamlineBootstrap.status());
                        System.out.println("Vulkan 1.4 / deviceAddressCommands / synchronization2 / RR device enabled; " + Bootstrap.serStatus());
                        require(VK10.vkDeviceWaitIdle(device) == VK10.VK_SUCCESS, "设备等待失败");
                    } finally {
                        StreamlineBootstrap.shutdown();
                        VK10.vkDestroyDevice(device, null);
                    }
                }
            } finally {
                VK10.vkDestroyInstance(instance, null);
            }
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
