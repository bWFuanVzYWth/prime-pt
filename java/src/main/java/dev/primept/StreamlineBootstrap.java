package dev.primept;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.invoke.MethodHandle;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Locale;
import org.lwjgl.sdl.SDLHints;
import org.lwjgl.system.Configuration;
import static java.lang.foreign.ValueLayout.*;

/** 进程级 SDK；先于任何 Vulkan 对象安装，最后一个 native renderer 释放后关闭。 */
public final class StreamlineBootstrap {
    private static boolean attempted, installed, available;
    private static String status = "尚未初始化";
    private static Path interposer;
    private static MethodHandle shutdown, error;

    private StreamlineBootstrap() {}

    public static boolean installed() { return installed; }
    public static boolean available() { return available; }
    public static String status() { return status; }

    /** 只设置 loader 和调用进程 bootstrap，不创建 Vulkan instance/device。 */
    public static void install() {
        if (attempted) return;
        attempted = true;
        if (!Boolean.parseBoolean(System.getProperty("primept.rr", "true"))) {
            unavailable("启动参数禁用；启用需要重启");
            return;
        }
        if (!System.getProperty("os.name", "").toLowerCase(Locale.ROOT).startsWith("windows")) {
            unavailable("RR 暂只支持 Windows");
            return;
        }
        try {
            var location = NativeRuntime.locate();
            var library = location.library();
            interposer = library.resolveSibling("sl.interposer.dll");
            for (String name : new String[] {"sl.interposer.dll", "sl.common.dll", "sl.dlss_d.dll", "nvngx_dlssd.dll"})
                if (!Files.isRegularFile(library.resolveSibling(name)))
                    throw new IllegalStateException("缺少 " + library.resolveSibling(name));
            var symbols = NativeRuntime.open(location, Arena.global());
            var linker = Linker.nativeLinker();
            error = linker.downcallHandle(symbols.find("prime_error").orElseThrow(), FunctionDescriptor.of(ADDRESS));
            var bootstrap = linker.downcallHandle(symbols.find("prime_streamline_bootstrap").orElseThrow(), FunctionDescriptor.of(JAVA_INT));
            shutdown = linker.downcallHandle(symbols.find("prime_streamline_shutdown").orElseThrow(), FunctionDescriptor.of(JAVA_INT));
            if ((int) bootstrap.invokeExact() != 0) throw failure();
            Configuration.VULKAN_LIBRARY_NAME.set(interposer.toString());
            installed = true;
            status = "SDK 已安装，等待宿主设备";
            System.getLogger("prime_pt").log(System.Logger.Level.INFO, "Streamline RR 早期初始化完成：" + interposer);
        } catch (Throwable failure) {
            unavailable("早期初始化失败：" + failure.getMessage());
            System.getLogger("prime_pt").log(System.Logger.Level.WARNING, "DLSS RR 不可用，继续原始 PT：" + status, failure);
        }
    }

    /** SDL3 在创建 Vulkan 窗口时会使用私有 loader，必须与 LWJGL 指向同一 interposer。 */
    public static void configureSdl() {
        if (installed && !SDLHints.SDL_SetHintWithPriority(SDLHints.SDL_HINT_VULKAN_LIBRARY,
                interposer.toString(), SDLHints.SDL_HINT_OVERRIDE))
            throw new IllegalStateException("无法设置 SDL 的 Streamline Vulkan loader");
    }

    static void deviceReady() {
        available = true;
        status = "SDK 与宿主设备已就绪";
    }

    static void unavailable(String reason) { available = false; status = reason; }

    /** 由 VulkanDevice.close 前置钩子调用；此时 renderer 和宿主提交均已完成。 */
    public static void shutdown() {
        if (!installed) return;
        try {
            if ((int) shutdown.invokeExact() != 0) throw failure();
            installed = false;
            unavailable("宿主设备已关闭");
        } catch (RuntimeException | Error failure) {
            throw failure;
        } catch (Throwable failure) {
            throw new IllegalStateException("Streamline 关闭失败", failure);
        }
    }

    private static IllegalStateException failure() throws Throwable {
        var message = (MemorySegment) error.invokeExact();
        return new IllegalStateException(message.address() == 0 ? "native Streamline 失败" : message.reinterpret(4096).getString(0));
    }
}
