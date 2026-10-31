package dev.primept.mixin;

import com.mojang.blaze3d.platform.NativeLibrariesBootstrap;
import dev.primept.StreamlineBootstrap;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** loader 选择先于宿主 VK.create，SDL 提示先于任何窗口。 */
@Mixin(NativeLibrariesBootstrap.class)
abstract class StreamlineBootstrapMixin {
    @Inject(method = "loadLibraries", at = @At("HEAD"))
    private static void primept$streamline(CallbackInfo callback) { StreamlineBootstrap.install(); }

    @Inject(method = "loadSdl", at = @At("RETURN"))
    private static void primept$sdl(CallbackInfo callback) { StreamlineBootstrap.configureSdl(); }
}
