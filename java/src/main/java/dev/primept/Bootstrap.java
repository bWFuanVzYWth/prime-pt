package dev.primept;

import com.mojang.renderpearl.backend.vulkan.VulkanFeatureSets;
import com.mojang.renderpearl.backend.vulkan.VulkanPhysicalDevice;
import com.mojang.renderpearl.backend.vulkan.init.FeatureSet;
import com.mojang.renderpearl.backend.vulkan.init.VulkanFeature;
import com.mojang.renderpearl.backend.vulkan.init.VulkanPNextStruct;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import org.lwjgl.system.MemoryStack;
import org.lwjgl.vulkan.*;

/** 在宿主 vkCreateDevice 前强制启用 RT/position fetch；SER 单独协商。 */
public final class Bootstrap {
    private static final List<String> EXTENSIONS = List.of(
            "VK_KHR_acceleration_structure", "VK_KHR_ray_tracing_pipeline", "VK_KHR_ray_query",
            "VK_KHR_deferred_host_operations", "VK_KHR_ray_tracing_position_fetch",
            "VK_KHR_device_address_commands");
    private static final VulkanFeature POSITION_FETCH = new VulkanFeature(
            new VulkanPNextStruct(VkPhysicalDeviceRayTracingPositionFetchFeaturesKHR.class),
            "rayTracingPositionFetch", VkPhysicalDeviceRayTracingPositionFetchFeaturesKHR.RAYTRACINGPOSITIONFETCH);
    private static final VulkanFeature DEVICE_COMMANDS = new VulkanFeature(
            new VulkanPNextStruct(VkPhysicalDeviceDeviceAddressCommandsFeaturesKHR.class),
            "deviceAddressCommands", VkPhysicalDeviceDeviceAddressCommandsFeaturesKHR.DEVICEADDRESSCOMMANDS);
    private static final VulkanFeature SYNCHRONIZATION = new VulkanFeature(
            VulkanFeatureSets.SYNC2_FEATURES_STRUCT, "synchronization2", VkPhysicalDeviceSynchronization2Features.SYNCHRONIZATION2);
    private static final List<String> RR_EXTENSIONS = List.of("VK_NVX_binary_import", "VK_NVX_image_view_handle",
            "VK_KHR_push_descriptor", "VK_KHR_buffer_device_address", "VK_KHR_synchronization2");
    private static final VulkanFeature PRIVATE_DATA = new VulkanFeature(
            new VulkanPNextStruct(VkPhysicalDevicePrivateDataFeatures.class), "privateData", VkPhysicalDevicePrivateDataFeatures.PRIVATEDATA);
    private static final Set<VulkanFeature> RR_FEATURES = Set.of(
            SYNCHRONIZATION,
            new VulkanFeature(VulkanFeatureSets.VK12_FEATURES_STRUCT, "timelineSemaphore", VkPhysicalDeviceVulkan12Features.TIMELINESEMAPHORE),
            new VulkanFeature(VulkanFeatureSets.VK12_FEATURES_STRUCT, "descriptorIndexing", VkPhysicalDeviceVulkan12Features.DESCRIPTORINDEXING),
            new VulkanFeature(VulkanFeatureSets.VK10_FEATURES_STRUCT, "shaderStorageImageExtendedFormats"),
            new VulkanFeature(VulkanFeatureSets.VK10_FEATURES_STRUCT, "shaderStorageImageWriteWithoutFormat"));
    private static final String SER_EXTENSION = "VK_EXT_ray_tracing_invocation_reorder";
    private static final VulkanFeature SER_FEATURE = new VulkanFeature(
            new VulkanPNextStruct(VkPhysicalDeviceRayTracingInvocationReorderFeaturesEXT.class),
            "rayTracingInvocationReorder", VkPhysicalDeviceRayTracingInvocationReorderFeaturesEXT.RAYTRACINGINVOCATIONREORDER);
    private static long rrPhysical;
    private static long serPhysical;
    private static long serDevice;
    private static String serStatus = "尚未协商宿主设备";
    private static int graphicsQueueIndex;
    private Bootstrap() {}

    /** 实例固定请求 Vulkan 1.4；缺少 loader 支持时在创建前失败。 */
    public static int instanceApi() {
        if (VK.getInstanceVersionSupported() < VK14.VK_API_VERSION_1_4)
            throw new IllegalStateException("Prime PT 需要 Vulkan 1.4 loader");
        return VK14.VK_API_VERSION_1_4;
    }

    /** 缺少能力时中止初始化，避免运行中静默退回非 RT 路径。 */
    public static FeatureSet enable(FeatureSet original, VulkanPhysicalDevice physical) {
        if (!physical.vkPhysicalDevice().getInstance().getCapabilities().Vulkan14
                || physical.vkPhysicalDeviceProperties().apiVersion() < VK14.VK_API_VERSION_1_4)
            throw new IllegalStateException("Prime PT 需要实际 Vulkan 1.4 instance 和物理设备");
        rrPhysical = 0;
        serPhysical = 0;
        serDevice = 0;
        serStatus = "缺少 EXT SER 扩展或特性";
        var graphics = physical.graphicsQueueFamilyAndIndex();
        if (graphics == null) throw new IllegalStateException("Prime PT 需要可呈现的 graphics/compute 队列");
        graphicsQueueIndex = graphics.rightInt();
        for (String extension : EXTENSIONS)
            if (!physical.hasDeviceExtension(extension))
                throw new IllegalStateException("Prime PT 缺少 " + extension);
        var extensions = new HashSet<>(original.extensions());
        extensions.addAll(EXTENSIONS);
        var features = new HashSet<>(original.features());
        try (var stack = MemoryStack.stackPush()) {
            var address = VkPhysicalDeviceVulkan12Features.calloc(stack).sType$Default();
            var commands = VkPhysicalDeviceDeviceAddressCommandsFeaturesKHR.calloc(stack).sType$Default();
            var synchronization = VkPhysicalDeviceSynchronization2Features.calloc(stack).sType$Default();
            var acceleration = VkPhysicalDeviceAccelerationStructureFeaturesKHR.calloc(stack).sType$Default();
            var pipeline = VkPhysicalDeviceRayTracingPipelineFeaturesKHR.calloc(stack).sType$Default();
            var query = VkPhysicalDeviceRayQueryFeaturesKHR.calloc(stack).sType$Default();
            var position = VkPhysicalDeviceRayTracingPositionFetchFeaturesKHR.calloc(stack).sType$Default();
            var available = VkPhysicalDeviceFeatures2.calloc(stack).sType$Default()
                    .pNext(address).pNext(commands).pNext(synchronization).pNext(acceleration).pNext(pipeline).pNext(query).pNext(position);
            VK12.vkGetPhysicalDeviceFeatures2(physical.vkPhysicalDevice(), available);
            if (!address.bufferDeviceAddress() || !acceleration.accelerationStructure() || !pipeline.rayTracingPipeline() || !query.rayQuery())
                throw new IllegalStateException("Prime PT 缺少 bufferDeviceAddress / accelerationStructure / rayTracingPipeline / rayQuery");
            if (!commands.deviceAddressCommands() || !synchronization.synchronization2())
                throw new IllegalStateException("Prime PT 需要 deviceAddressCommands / synchronization2");
            if (!position.rayTracingPositionFetch())
                throw new IllegalStateException("Prime PT 需要 rayTracingPositionFetch；不保留 shader 顶点副本回退");
            if (!available.features().shaderStorageImageExtendedFormats())
                throw new IllegalStateException("Prime PT 降噪辅助量需要 shaderStorageImageExtendedFormats");
            boolean supported = physical.hasDeviceExtension(SER_EXTENSION);
            if (supported) {
                var reorder = VkPhysicalDeviceRayTracingInvocationReorderFeaturesEXT.calloc(stack).sType$Default();
                var ser = VkPhysicalDeviceFeatures2.calloc(stack).sType$Default().pNext(reorder);
                VK12.vkGetPhysicalDeviceFeatures2(physical.vkPhysicalDevice(), ser);
                supported = reorder.rayTracingInvocationReorder();
            }
            if (supported) {
                extensions.add(SER_EXTENSION);
                features.add(SER_FEATURE);
                serPhysical = physical.vkPhysicalDevice().address();
                serStatus = "等待宿主逻辑设备确认";
            } else {
                System.getLogger("prime_pt").log(System.Logger.Level.WARNING,
                        "SER 不可用：缺少 EXT 扩展或特性，使用普通 RT pipeline");
            }
            if (StreamlineBootstrap.installed()) {
                var privateData = VkPhysicalDevicePrivateDataFeatures.calloc(stack).sType$Default();
                var rrSync = VkPhysicalDeviceSynchronization2Features.calloc(stack).sType$Default();
                var streamline = VkPhysicalDeviceFeatures2.calloc(stack).sType$Default()
                        .pNext(address).pNext(privateData).pNext(rrSync);
                VK12.vkGetPhysicalDeviceFeatures2(physical.vkPhysicalDevice(), streamline);
                if (physical.vkPhysicalDeviceProperties().apiVersion() < VK13.VK_API_VERSION_1_3 || !privateData.privateData())
                    throw new IllegalStateException("已安装的 Streamline loader 需要 Vulkan 1.3 privateData");
                features.add(PRIVATE_DATA);
                if (RR_EXTENSIONS.stream().allMatch(physical::hasDeviceExtension)
                        && address.timelineSemaphore() && address.descriptorIndexing() && rrSync.synchronization2()
                        && streamline.features().shaderStorageImageExtendedFormats()
                        && streamline.features().shaderStorageImageWriteWithoutFormat()) {
                    extensions.addAll(RR_EXTENSIONS);
                    features.addAll(RR_FEATURES);
                    rrPhysical = physical.vkPhysicalDevice().address();
                } else {
                    StreamlineBootstrap.unavailable("宿主设备缺少 RR 扩展或特性");
                    System.getLogger("prime_pt").log(System.Logger.Level.WARNING, "DLSS RR 不可用：" + StreamlineBootstrap.status());
                }
            }
        }
        features.add(new VulkanFeature(VulkanFeatureSets.VK12_FEATURES_STRUCT, "bufferDeviceAddress", VkPhysicalDeviceVulkan12Features.BUFFERDEVICEADDRESS));
        features.add(new VulkanFeature(VulkanFeatureSets.VK10_FEATURES_STRUCT, "shaderStorageImageExtendedFormats"));
        features.add(new VulkanFeature(new VulkanPNextStruct(VkPhysicalDeviceAccelerationStructureFeaturesKHR.class),
                "accelerationStructure", VkPhysicalDeviceAccelerationStructureFeaturesKHR.ACCELERATIONSTRUCTURE));
        features.add(new VulkanFeature(new VulkanPNextStruct(VkPhysicalDeviceRayTracingPipelineFeaturesKHR.class),
                "rayTracingPipeline", VkPhysicalDeviceRayTracingPipelineFeaturesKHR.RAYTRACINGPIPELINE));
        features.add(new VulkanFeature(new VulkanPNextStruct(VkPhysicalDeviceRayQueryFeaturesKHR.class),
                "rayQuery", VkPhysicalDeviceRayQueryFeaturesKHR.RAYQUERY));
        features.add(POSITION_FETCH);
        features.add(DEVICE_COMMANDS);
        features.add(SYNCHRONIZATION);
        return new FeatureSet(original.name(), Set.copyOf(extensions), Set.copyOf(features), original.condition());
    }

    /** 只报告当前逻辑设备真正启用的能力，不能以物理支持替代。 */
    public static void created(FeatureSet enabled, VkDevice device) {
        if (!device.getPhysicalDevice().getInstance().getCapabilities().Vulkan14
                || !enabled.extensions().containsAll(EXTENSIONS) || !enabled.features().contains(POSITION_FETCH)
                || !enabled.features().contains(DEVICE_COMMANDS) || !enabled.features().contains(SYNCHRONIZATION))
            throw new IllegalStateException("宿主逻辑设备未启用强制 RT / position fetch 能力");
        serDevice = 0;
        if (serPhysical == device.getPhysicalDevice().address() && enabled.extensions().contains(SER_EXTENSION)
                && enabled.features().contains(SER_FEATURE)) {
            serDevice = device.address();
            serStatus = "EXT SER 设备能力已启用";
            System.getLogger("prime_pt").log(System.Logger.Level.INFO, "EXT SER 宿主 Vulkan 特性已启用");
        } else if (serPhysical != 0) {
            serStatus = "宿主逻辑设备未启用 SER";
            System.getLogger("prime_pt").log(System.Logger.Level.WARNING,
                    "宿主逻辑设备未启用 SER，使用普通 RT pipeline");
        }
        if (rrPhysical == device.getPhysicalDevice().address() && enabled.extensions().containsAll(RR_EXTENSIONS)
                && enabled.features().containsAll(RR_FEATURES) && enabled.features().contains(PRIVATE_DATA)) {
            StreamlineBootstrap.deviceReady();
            System.getLogger("prime_pt").log(System.Logger.Level.INFO, "Streamline RR 宿主 Vulkan 特性已启用");
        }
    }

    /** 由宿主设备协商捕获，与 graphicsQueue 的 family 一并传给 SDK。 */
    public static int graphicsQueueIndex() { return graphicsQueueIndex; }

    /** 只在创建回调确认该逻辑设备已启用后返回 true。 */
    public static boolean invocationReorder(VkDevice device) { return serDevice != 0 && serDevice == device.address(); }

    /** 设置界面和命令显示的实际设备协商状态。 */
    public static String serStatus() { return serStatus; }
}
