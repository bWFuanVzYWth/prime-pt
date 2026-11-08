package dev.primept.mixin;

import com.mojang.blaze3d.platform.Window;
import dev.primept.WindowDiagnostic;
import org.lwjgl.sdl.SDLVideo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.Redirect;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** 保留宿主全屏语义，仅为实际卡死保留窗口调用的成对证据。 */
@Mixin(Window.class)
abstract class WindowMixin {
    @Inject(method = "setMode", at = @At("HEAD"))
    private void primept$modeBegin(CallbackInfo callback) { WindowDiagnostic.mark("setMode BEGIN"); }
    @Inject(method = "setMode", at = @At("RETURN"))
    private void primept$modeEnd(CallbackInfo callback) { WindowDiagnostic.mark("setMode END"); }
    @Redirect(method = "applyWindowed", at = @At(value = "INVOKE", target = "Lorg/lwjgl/sdl/SDLVideo;SDL_SetWindowFullscreen(JZ)Z"))
    private boolean primept$leaveFullscreen(long window, boolean fullscreen) {
        WindowDiagnostic.mark("SDL_SetWindowFullscreen(false) BEGIN");
        boolean result = SDLVideo.SDL_SetWindowFullscreen(window, fullscreen);
        WindowDiagnostic.mark("SDL_SetWindowFullscreen(false) END result=" + result);
        return result;
    }
    @Redirect(method = "syncWindow", at = @At(value = "INVOKE", target = "Lorg/lwjgl/sdl/SDLVideo;SDL_SyncWindow(J)Z"))
    private boolean primept$sync(long window) {
        WindowDiagnostic.mark("SDL_SyncWindow BEGIN");
        boolean result = SDLVideo.SDL_SyncWindow(window);
        WindowDiagnostic.mark("SDL_SyncWindow END result=" + result);
        return result;
    }
}
