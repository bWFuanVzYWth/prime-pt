package dev.primept;

import com.mojang.renderpearl.backend.vulkan.init.FeatureSet;
import java.util.List;
import java.util.Set;
import static org.lwjgl.vulkan.EXTRayTracingInvocationReorder.*;
import org.lwjgl.vulkan.VK12;
import org.lwjgl.vulkan.VK13;
import org.lwjgl.vulkan.VK14;

/** 无窗口检查能力不足的发布边界；不创建设备，不加载 native renderer。 */
final class StartupSupportFixture {
    public static void main(String[] arguments) {
        preservesHostApiWhenLoaderIsTooOld();
        selectsReorderOnlyWhenEffective();
        missingCapabilitiesKeepVanillaFeatures();
        openGlKeepsItsOwnReason();
        SupportNotice.supported();
        System.out.println("Startup support fallback fixture passed");
    }

    private static void selectsReorderOnlyWhenEffective() {
        require(!Bootstrap.useReorder(true, VK_RAY_TRACING_INVOCATION_REORDER_MODE_NONE_EXT),
                "兼容性扩展的 NONE 提示应选择 KHR 光追");
        require(!Bootstrap.useReorder(false, VK_RAY_TRACING_INVOCATION_REORDER_MODE_REORDER_EXT),
                "物理属性不能替代逻辑设备特性");
        require(Bootstrap.useReorder(true, VK_RAY_TRACING_INVOCATION_REORDER_MODE_REORDER_EXT),
                "支持实际重排时保留 SER");
    }

    private static void preservesHostApiWhenLoaderIsTooOld() {
        require(Bootstrap.instanceApi(VK12.VK_API_VERSION_1_2, VK13.VK_API_VERSION_1_3)
                == VK12.VK_API_VERSION_1_2, "旧 loader 必须仍可创建原版实例");
        require(Bootstrap.instanceApi(VK12.VK_API_VERSION_1_2, VK14.VK_API_VERSION_1_4)
                == VK14.VK_API_VERSION_1_4, "兼容 loader 为 Prime 请求 Vulkan 1.4");
    }

    private static void missingCapabilitiesKeepVanillaFeatures() {
        var original = new FeatureSet("Vanilla", Set.of("VK_KHR_swapchain"), Set.of());
        var missing = List.of("VK_KHR_ray_tracing_position_fetch", "deviceAddressCommands");
        var selected = Bootstrap.unsupported(original, missing);
        require(selected == original, "不完整的 Prime 能力不得混入原版设备请求");
        require(!Bootstrap.rayTracingEnabled(), "失败后不得准备 native 资源或专用纹理");
        require(SupportNotice.current().missing().equals(missing), "保留完整缺失项供日志和通知使用");
    }

    private static void openGlKeepsItsOwnReason() {
        SupportNotice.openGl();
        var notice = SupportNotice.current();
        require(notice.openGl() && notice.missing().isEmpty(), "OpenGL 提示不冒充显卡扩展不足");
        SupportNotice.openGl();
        require(SupportNotice.current() == notice, "相同错误不得逐帧替换通知或重复记日志");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
