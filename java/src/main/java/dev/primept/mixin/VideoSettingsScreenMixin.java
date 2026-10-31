package dev.primept.mixin;

import dev.primept.VideoOptions;
import net.minecraft.client.Options;
import net.minecraft.client.gui.components.StringWidget;
import net.minecraft.client.gui.layouts.LinearLayout;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.gui.screens.options.OptionsSubScreen;
import net.minecraft.client.gui.screens.options.VideoSettingsScreen;
import net.minecraft.network.chat.Component;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.Unique;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(VideoSettingsScreen.class)
public abstract class VideoSettingsScreenMixin extends OptionsSubScreen {
    @Shadow @Final private LinearLayout header;
    @Shadow private StringWidget restartWarning;
    @Unique private VideoOptions primept$options;

    protected VideoSettingsScreenMixin(Screen parent, Options options, Component title) {
        super(parent, options, title);
    }

    @Override
    protected void init() {
        // 原生布局保留 children，重建前清空，避免 resize 或恢复默认后重复追加。
        layout.removeChildren();
        header.removeChildren();
        restartWarning = null;
        super.init();
    }

    @Inject(method = "addOptions()V", at = @At("HEAD"))
    private void primept$addOptions(CallbackInfo callback) {
        primept$options = VideoOptions.addTo(list, this, this::rebuildWidgets);
    }

    @Inject(method = "tick()V", at = @At("TAIL"))
    private void primept$tick(CallbackInfo callback) {
        if (primept$options != null) primept$options.tick();
    }
}
