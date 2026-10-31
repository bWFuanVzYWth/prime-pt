package dev.primept.mixin;

import com.mojang.brigadier.CommandDispatcher;
import dev.primept.CommandHints;
import dev.primept.Settings;
import net.minecraft.client.multiplayer.ClientPacketListener;
import net.minecraft.client.multiplayer.ClientSuggestionProvider;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Unique;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(ClientPacketListener.class)
abstract class CommandMixin {
    @Unique private CommandDispatcher<ClientSuggestionProvider> primept$hintDispatcher;

    @Inject(method = "getCommands", at = @At("RETURN"))
    private void primept$hints(CallbackInfoReturnable<CommandDispatcher<ClientSuggestionProvider>> callback) {
        var dispatcher = callback.getReturnValue();
        if (dispatcher == primept$hintDispatcher) return;
        CommandHints.install(dispatcher);
        primept$hintDispatcher = dispatcher;
    }

    @Inject(method = "sendCommand", at = @At("HEAD"), cancellable = true)
    private void primept$command(String input, CallbackInfo callback) {
        if (Settings.command(input)) callback.cancel();
    }
}
