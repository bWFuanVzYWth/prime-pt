package dev.primept.mixin;

import dev.primept.FrameChartHud;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.components.DebugScreenOverlay;
import net.minecraft.client.gui.components.debugchart.FpsDebugChart;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Redirect;

/** 沿用 F3+2 开关，仅替代原版 FPS 图；右侧 TPS 图和其余 F3 内容由宿主绘制。 */
@Mixin(DebugScreenOverlay.class)
abstract class FrameChartMixin {
    @Redirect(method = "extractRenderState", at = @At(value = "INVOKE", target =
            "Lnet/minecraft/client/gui/components/debugchart/FpsDebugChart;extractRenderState(Lnet/minecraft/client/gui/GuiGraphicsExtractor;III)V"))
    private void primept$frameChart(FpsDebugChart vanilla, GuiGraphicsExtractor graphics, int x, int width, int height) {
        if (!FrameChartHud.draw(graphics, x, height)) vanilla.extractRenderState(graphics, x, width, height);
    }
}
