package dev.primept;

import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.GuiGraphicsExtractor;

/** Minecraft 26.3 的 F3+2 画布适配，不改变宿主帧率或 TPS 的计时器。 */
public final class FrameChartHud {
    private FrameChartHud() {}

    /** 有世界和 native 有效帧时替代原版 FPS 图，其余情况保留宿主图。 */
    public static boolean draw(GuiGraphicsExtractor graphics) {
        var minecraft = Minecraft.getInstance();
        if (minecraft.level == null || !FrameChart.ready()) return false;
        var window = minecraft.getWindow();
        double mx = minecraft.mouseHandler.isMouseGrabbed() ? -1
                : minecraft.mouseHandler.xpos() * graphics.guiWidth() / window.getScreenWidth();
        double my = minecraft.mouseHandler.isMouseGrabbed() ? -1
                : minecraft.mouseHandler.ypos() * graphics.guiHeight() / window.getScreenHeight();
        return FrameChart.paint(new FrameChart.Surface() {
            public void fill(int x0, int y0, int x1, int y1, int color) { graphics.fill(x0, y0, x1, y1, color); }
            public void text(String text, int x, int y, int color) { graphics.text(minecraft.font, text, x, y, color, false); }
            public int textWidth(String text) { return minecraft.font.width(text); }
        }, 0, graphics.guiWidth() / 2, graphics.guiHeight(), mx, my);
    }
}
