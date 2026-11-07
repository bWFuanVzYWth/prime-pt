package dev.primept;

import com.mojang.renderpearl.backend.vulkan.VulkanFeatureSets;
import com.mojang.renderpearl.backend.vulkan.VulkanPhysicalDevice;
import com.mojang.renderpearl.backend.vulkan.init.FeatureSet;
import com.mojang.renderpearl.backend.vulkan.init.VulkanFeature;
import com.mojang.renderpearl.backend.vulkan.init.VulkanPNextStruct;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import org.lwjgl.system.MemoryStack;
import org.lwjgl.vulkan.*;

/** 在宿主 vkCreateDevice 前完整协商 RT；缺少必要能力时保留原版设备，SER 单独协商。 */
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
    private static final VulkanFeature SUBGROUP = new VulkanFeature(
            new VulkanPNextStruct(VkPhysicalDeviceSubgroupSizeControlFeatures.class),
            "subgroupSizeControl", VkPhysicalDeviceSubgroupSizeControlFeatures.SUBGROUPSIZECONTROL);
    private static final Set<VulkanFeature> PT_FEATURES = Set.of(
            new VulkanFeature(VulkanFeatureSets.VK12_FEATURES_STRUCT, "bufferDeviceAddress", VkPhysicalDeviceVulkan12Features.BUFFERDEVICEADDRESS),
            new VulkanFeature(VulkanFeatureSets.VK10_FEATURES_STRUCT, "shaderStorageImageExtendedFormats"),
            new VulkanFeature(new VulkanPNextStruct(VkPhysicalDeviceAccelerationStructureFeaturesKHR.class),
                    "accelerationStructure", VkPhysicalDeviceAccelerationStructureFeaturesKHR.ACCELERATIONSTRUCTURE),
            new VulkanFeature(new VulkanPNextStruct(VkPhysicalDeviceRayTracingPipelineFeaturesKHR.class),
                    "rayTracingPipeline", VkPhysicalDeviceRayTracingPipelineFeaturesKHR.RAYTRACINGPIPELINE),
            new VulkanFeature(new VulkanPNextStruct(VkPhysicalDeviceRayQueryFeaturesKHR.class),
                    "rayQuery", VkPhysicalDeviceRayQueryFeaturesKHR.RAYQUERY),
            POSITION_FETCH, DEVICE_COMMANDS, SYNCHRONIZATION, SUBGROUP);
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
    private static final String CHECKPOINT_EXTENSION = "VK_NV_device_diagnostic_checkpoints";
    private static final VulkanFeature SER_FEATURE = new VulkanFeature(
            new VulkanPNextStruct(VkPhysicalDeviceRayTracingInvocationReorderFeaturesEXT.class),
            "rayTracingInvocationReorder", VkPhysicalDeviceRayTracingInvocationReorderFeaturesEXT.RAYTRACINGINVOCATIONREORDER);
    private static long rrPhysical;
    private static long ptPhysical;
    private static long ptDevice;
    private static long serPhysical;
    private static long serDevice;
    private static long checkpointDevice;
    private static String serStatus = "尚未协商宿主设备";
    private static int graphicsQueueIndex;
    private Bootstrap() {}

    /** loader 支持时请求 Vulkan 1.4，否则保留原版版本，使原版渲染仍可启动。 */
    public static int instanceApi(int original) {
        return instanceApi(original, VK.getInstanceVersionSupported());
    }

    static int instanceApi(int original, int supported) {
        return supported >= VK14.VK_API_VERSION_1_4 ? Math.max(original, VK14.VK_API_VERSION_1_4) : original;
    }

    static FeatureSet unsupported(FeatureSet original, List<String> missing) {
        ptPhysical = ptDevice = 0;
        serStatus = "Prime PT 所需设备能力不可用";
        StreamlineBootstrap.unavailable("Prime PT 所需设备能力不可用");
        SupportNotice.missing(missing);
        return original;
    }

    /** 缺少能力时不追加 RT 扩展或特性，原因交给界面通知；仅完整支持后允许 native 接入。 */
    public static FeatureSet enable(FeatureSet original, VulkanPhysicalDevice physical) {
        ptPhysical = ptDevice = 0;
        rrPhysical = 0;
        serPhysical = 0;
        serDevice = 0;
        checkpointDevice = 0;
        serStatus = "缺少 EXT SER 扩展或特性";
        // 已安装的 interposer 仍负责原版设备；保留其可用的 privateData 特性。
        if (StreamlineBootstrap.installed()
                && physical.vkPhysicalDeviceProperties().apiVersion() >= VK13.VK_API_VERSION_1_3) {
            try (var stack = MemoryStack.stackPush()) {
                var privateData = VkPhysicalDevicePrivateDataFeatures.calloc(stack).sType$Default();
                var available = VkPhysicalDeviceFeatures2.calloc(stack).sType$Default().pNext(privateData);
                VK12.vkGetPhysicalDeviceFeatures2(physical.vkPhysicalDevice(), available);
                if (privateData.privateData()) {
                    var features = new HashSet<>(original.features());
                    features.add(PRIVATE_DATA);
                    original = new FeatureSet(original.name(), original.extensions(), Set.copyOf(features), original.condition());
                }
            }
        }
        var missing = new ArrayList<String>();
        if (!physical.vkPhysicalDevice().getInstance().getCapabilities().Vulkan14)
            missing.add("Vulkan 1.4 instance/loader");
        if (physical.vkPhysicalDeviceProperties().apiVersion() < VK14.VK_API_VERSION_1_4)
            missing.add("Vulkan 1.4 device");
        var graphics = physical.graphicsQueueFamilyAndIndex();
        if (graphics == null) missing.add("graphics/compute queue");
        for (String extension : EXTENSIONS)
            if (!physical.hasDeviceExtension(extension)) missing.add(extension);
        if (!missing.isEmpty()) return unsupported(original, missing);
        graphicsQueueIndex = graphics.rightInt();
        var extensions = new HashSet<>(original.extensions());
        extensions.addAll(EXTENSIONS);
        if ("1".equals(System.getenv("PRIME_PT_DIAGNOSTIC_CHECKPOINTS"))) {
            if (physical.hasDeviceExtension(CHECKPOINT_EXTENSION)) extensions.add(CHECKPOINT_EXTENSION);
            else System.getLogger("prime_pt").log(System.Logger.Level.WARNING,
                    "异步 GPU 检查点不可用：缺少 " + CHECKPOINT_EXTENSION);
        }
        var features = new HashSet<>(original.features());
        try (var stack = MemoryStack.stackPush()) {
            var address = VkPhysicalDeviceVulkan12Features.calloc(stack).sType$Default();
            var commands = VkPhysicalDeviceDeviceAddressCommandsFeaturesKHR.calloc(stack).sType$Default();
            var synchronization = VkPhysicalDeviceSynchronization2Features.calloc(stack).sType$Default();
            var acceleration = VkPhysicalDeviceAccelerationStructureFeaturesKHR.calloc(stack).sType$Default();
            var pipeline = VkPhysicalDeviceRayTracingPipelineFeaturesKHR.calloc(stack).sType$Default();
            var query = VkPhysicalDeviceRayQueryFeaturesKHR.calloc(stack).sType$Default();
            var position = VkPhysicalDeviceRayTracingPositionFetchFeaturesKHR.calloc(stack).sType$Default();
            var subgroup = VkPhysicalDeviceSubgroupSizeControlFeatures.calloc(stack).sType$Default();
            var available = VkPhysicalDeviceFeatures2.calloc(stack).sType$Default()
                    .pNext(address).pNext(commands).pNext(synchronization).pNext(acceleration).pNext(pipeline).pNext(query).pNext(position).pNext(subgroup);
            VK12.vkGetPhysicalDeviceFeatures2(physical.vkPhysicalDevice(), available);
            if (!address.bufferDeviceAddress()) missing.add("bufferDeviceAddress");
            if (!acceleration.accelerationStructure()) missing.add("accelerationStructure");
            if (!pipeline.rayTracingPipeline()) missing.add("rayTracingPipeline");
            if (!query.rayQuery()) missing.add("rayQuery");
            if (!commands.deviceAddressCommands()) missing.add("deviceAddressCommands");
            if (!synchronization.synchronization2()) missing.add("synchronization2");
            if (!position.rayTracingPositionFetch()) missing.add("rayTracingPositionFetch");
            if (!subgroup.subgroupSizeControl()) missing.add("subgroupSizeControl");
            if (!available.features().shaderStorageImageExtendedFormats()) missing.add("shaderStorageImageExtendedFormats");
            var subgroupOps = VkPhysicalDeviceSubgroupProperties.calloc(stack).sType$Default();
            var subgroupSize = VkPhysicalDeviceSubgroupSizeControlProperties.calloc(stack).sType$Default();
            var properties = VkPhysicalDeviceProperties2.calloc(stack).sType$Default().pNext(subgroupOps).pNext(subgroupSize);
            VK12.vkGetPhysicalDeviceProperties2(physical.vkPhysicalDevice(), properties);
            int operations = VK11.VK_SUBGROUP_FEATURE_BASIC_BIT | VK11.VK_SUBGROUP_FEATURE_BALLOT_BIT | VK11.VK_SUBGROUP_FEATURE_ARITHMETIC_BIT;
            if (subgroupSize.minSubgroupSize() > 32 || subgroupSize.maxSubgroupSize() < 32
                    || (subgroupSize.requiredSubgroupSizeStages() & VK10.VK_SHADER_STAGE_COMPUTE_BIT) == 0
                    || (subgroupOps.supportedStages() & VK10.VK_SHADER_STAGE_COMPUTE_BIT) == 0
                    || (subgroupOps.supportedOperations() & operations) != operations)
                missing.add("compute subgroup32 BASIC/BALLOT/ARITHMETIC");
            if (!missing.isEmpty()) return unsupported(original, missing);
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
                    return unsupported(original, List.of("Streamline privateData"));
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
        features.addAll(PT_FEATURES);
        ptPhysical = physical.vkPhysicalDevice().address();
        SupportNotice.supported();
        return new FeatureSet(original.name(), Set.copyOf(extensions), Set.copyOf(features), original.condition());
    }

    /** 只报告当前逻辑设备真正启用的能力，不能以物理支持替代。 */
    public static void created(FeatureSet enabled, VkDevice device) {
        ptDevice = 0;
        checkpointDevice = 0;
        if (ptPhysical != device.getPhysicalDevice().address()) return;
        if (!device.getPhysicalDevice().getInstance().getCapabilities().Vulkan14
                || !enabled.extensions().containsAll(EXTENSIONS) || !enabled.features().containsAll(PT_FEATURES)) {
            unsupported(enabled, List.of("RT / position fetch device features"));
            return;
        }
        ptDevice = device.address();
        if ("1".equals(System.getenv("PRIME_PT_DIAGNOSTIC_CHECKPOINTS"))
                && enabled.extensions().contains(CHECKPOINT_EXTENSION)
                && device.getCapabilities().VK_NV_device_diagnostic_checkpoints) {
            checkpointDevice = device.address();
            System.getLogger("prime_pt").log(System.Logger.Level.INFO, "NV 异步 GPU 检查点已启用；保留原提交顺序");
        }
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

    /** 实际逻辑设备已经完整启用 Prime 必需能力，才允许资源准备和专用纹理 usage。 */
    public static boolean rayTracingEnabled() { return ptDevice != 0; }

    /** 只在创建回调确认该逻辑设备已启用后返回 true。 */
    public static boolean invocationReorder(VkDevice device) { return serDevice != 0 && serDevice == device.address(); }

    /** 仅显式诊断请求且逻辑设备实际启用 NV 检查点时返回 true。 */
    public static boolean diagnosticCheckpoints(VkDevice device) { return checkpointDevice != 0 && checkpointDevice == device.address(); }

    /** 设置界面和命令显示的实际设备协商状态。 */
    public static String serStatus() { return serStatus; }
}
