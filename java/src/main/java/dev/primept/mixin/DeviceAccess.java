package dev.primept.mixin;

import com.mojang.renderpearl.backend.api.GpuDeviceBackend;
import com.mojang.renderpearl.frontend.FrontendGpuDevice;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;

/** 宿主设备仍由 RenderSystem 独占。 */
@Mixin(FrontendGpuDevice.class)
public interface DeviceAccess {
    /** 只借用宿主 backend，不改变其生命周期。 */
    @Accessor("backend") GpuDeviceBackend primept$backend();
}
