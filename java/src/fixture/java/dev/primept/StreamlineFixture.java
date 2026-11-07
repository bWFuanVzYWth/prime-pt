package dev.primept;

import org.lwjgl.sdl.SDLInit;
import org.lwjgl.sdl.SDLVulkan;
import org.lwjgl.vulkan.*;
import org.lwjgl.system.MemoryStack;
import com.mojang.renderpearl.backend.vulkan.VulkanBackend;
import com.mojang.renderpearl.backend.vulkan.VulkanFeatureSets;
import com.mojang.renderpearl.backend.vulkan.VulkanPhysicalDevice;
import com.mojang.renderpearl.backend.vulkan.init.FeatureSet;
import java.util.Set;
import java.nio.ByteBuffer;
import java.nio.file.Path;

/** 无窗口验证 early interposer、Vulkan 1.4 与宿主真实 feature 创建链。 */
final class StreamlineFixture {
    public static void main(String[] arguments) throws Exception {
        if (arguments.length == 1 && arguments[0].equals("--bundled"))
            NativeRuntime.locateBundled(Path.of(".primept/runtime"));
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
                device(java.util.List.of(arguments).contains("--unsupported"));
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

    private static void device(boolean unsupported) throws Exception {
        try (var stack = MemoryStack.stackPush()) {
            var application = VkApplicationInfo.calloc(stack).sType$Default().apiVersion(Bootstrap.instanceApi(VK12.VK_API_VERSION_1_2));
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
                try (var physical = new VulkanPhysicalDevice(new VkPhysicalDevice(devices.get(0), instance)) {
                    @Override public boolean hasDeviceExtension(String extension) {
                        return !(unsupported && extension.equals("VK_KHR_ray_tracing_position_fetch"))
                                && super.hasDeviceExtension(extension);
                    }
                }) {
                    var original = unsupported
                            ? new FeatureSet("Vanilla fixture", VulkanFeatureSets.requiredFeatureSets())
                            : new FeatureSet("Prime fixture", Set.of(), Set.of());
                    var enabled = Bootstrap.enable(original, physical);
                    var create = VulkanBackend.class.getDeclaredMethod("createDevice", FeatureSet.class, VulkanPhysicalDevice.class);
                    create.setAccessible(true);
                    var device = (VkDevice) create.invoke(null, enabled, physical);
                    try {
                        Bootstrap.created(enabled, device);
                        if (unsupported) {
                            require(!Bootstrap.rayTracingEnabled(), "扩展不足时不得发布 Prime 设备");
                            require(enabled.extensions().equals(original.extensions()), "原版设备不得带入不支持的 RT 扩展");
                            require(SupportNotice.current().missing().contains("VK_KHR_ray_tracing_position_fetch"), "保留真实缺失扩展供常驻通知使用");
                            System.out.println("Missing RT extension kept a working vanilla Vulkan device; native renderer was not created");
                            return;
                        }
                        require(Bootstrap.rayTracingEnabled(), "Prime 必需能力完整启用后才可准备资源");
                        require(device.getCapabilities().Vulkan14, "逻辑设备实际未使用 Vulkan 1.4");
                        require(device.getCapabilities().VK_KHR_device_address_commands, "deviceAddressCommands 扩展未启用");
                        require(StreamlineBootstrap.available(), StreamlineBootstrap.status());
                        System.out.println("Vulkan 1.4 / deviceAddressCommands / synchronization2 / RR device enabled; " + Bootstrap.serStatus());
                        nativeQueries(instance, physical, device);
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

    /** 执行生产FFM wrapper；空续接允许查询且不修改宿主缓冲。 */
    private static void nativeQueries(VkInstance instance, VulkanPhysicalDevice physical, VkDevice device) {
        var graphics = physical.graphicsQueueFamilyAndIndex();
        try (var stack = MemoryStack.stackPush()) {
            var queue = stack.mallocPointer(1);
            VK10.vkGetDeviceQueue(device, graphics.leftInt(), graphics.rightInt(), queue);
            try (var renderer = new Native(instance.address(), physical.vkPhysicalDevice().address(),
                    device.address(), queue.get(0), graphics.leftInt(), graphics.rightInt(),
                    Bootstrap.invocationReorder(device), false)) {
                renderer.beginTints(0);
                var output = ByteBuffer.allocateDirect(48);
                for (int i = 0; i < output.capacity(); i++) output.put(i, (byte) 0x5a);
                output.position(8).limit(32);
                for (ByteBuffer destination : new ByteBuffer[] {null, output}) {
                    require(renderer.biomeRequests(destination) == 0, "空biome续接应返回0条请求");
                    require(renderer.tintRequests(destination) == 0, "空自定义tint续接应返回0条请求");
                }
                require(output.position() == 8 && output.limit() == 32, "FFM查询不得改变buffer范围");
                output.clear();
                for (int i = 0; i < output.capacity(); i++) require(output.get(i) == (byte) 0x5a, "空查询不得写入buffer");
                System.out.println("Native biome/tint FFM null and direct-buffer queries passed");
            }
        }
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
