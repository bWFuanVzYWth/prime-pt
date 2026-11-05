package dev.primept.mixin;

import dev.primept.Settings;

import com.mojang.renderpearl.api.pipeline.RenderPipeline;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.Hud;
import net.minecraft.client.renderer.RenderPipelines;
import net.minecraft.resources.Identifier;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Redirect;

/** 保留宿主的准星可见性与资源包图案，使用描边保证亮暗背景都有对比。 */
@Mixin(Hud.class)
abstract class CrosshairMixin {
    @Redirect(method = "extractCrosshair", at = @At(value = "INVOKE", ordinal = 0, target =
            "Lnet/minecraft/client/gui/GuiGraphicsExtractor;blitSprite(Lcom/mojang/renderpearl/api/pipeline/RenderPipeline;Lnet/minecraft/resources/Identifier;IIII)V"))
    private void primept$drawCrosshair(GuiGraphicsExtractor graphics, RenderPipeline pipeline,
            Identifier sprite, int x, int y, int width, int height) {
        if (!Settings.enabled()) {
            graphics.blitSprite(pipeline, sprite, x, y, width, height);
            return;
        }
        // 原版反相混合在中灰处没有对比。描边与图案分层，确保重叠像素由图案覆盖。
        var textured = RenderPipelines.GUI_TEXTURED;
        int outline = 0xff000000;
        graphics.blitSprite(textured, sprite, x - 1, y, width, height, outline);
        graphics.blitSprite(textured, sprite, x + 1, y, width, height, outline);
        graphics.blitSprite(textured, sprite, x, y - 1, width, height, outline);
        graphics.blitSprite(textured, sprite, x, y + 1, width, height, outline);
        graphics.nextStratum();
        graphics.blitSprite(textured, sprite, x, y, width, height);
    }
}
