package dev.primept;

import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.GuiGraphicsExtractor;

/** Minecraft 26.3 的 F3+2 画布适配，不改变宿主帧率或 TPS 的计时器。 */
public final class FrameChartHud {
    private FrameChartHud() {}

    /** 有世界和 native 有效帧时替代原版 FPS 图，沿用宿主图表坐标系。 */
    public static boolean draw(GuiGraphicsExtractor graphics, int x, int bottom) {
        var minecraft = Minecraft.getInstance();
        if (!Renderer.active() || minecraft.level == null) return false;
        FrameChart.requestSnapshot();
        if (!FrameChart.ready()) return false;
        var window = minecraft.getWindow();
        // F3 可独立缩小 pose。宿主的 bottom 已在调试坐标中，普通 guiHeight 仍按常规 GUI 缩放。
        float poseScale = graphics.pose().m00();
        int viewportWidth = poseScale == 1 ? graphics.guiWidth()
                : window.getWidth() / Math.round(window.getGuiScale() * poseScale);
        double mx = -1, my = -1;
        if (!minecraft.mouseHandler.isMouseGrabbed() && window.getScreenWidth() > 0 && window.getScreenHeight() > 0) {
            mx = minecraft.mouseHandler.xpos() * graphics.guiWidth() / window.getScreenWidth();
            my = minecraft.mouseHandler.ypos() * graphics.guiHeight() / window.getScreenHeight();
        }
        return paint(new FrameChart.Surface() {
            public void fill(int x0, int y0, int x1, int y1, int color) { graphics.fill(x0, y0, x1, y1, color); }
            public void text(String text, int x, int y, int color) { graphics.text(minecraft.font, text, x, y, color, false); }
            public int textWidth(String text) { return minecraft.font.width(text); }
        }, x, viewportWidth, bottom, mx, my, poseScale);
    }

    static boolean paint(FrameChart.Surface surface, int x, int viewportWidth, int bottom,
            double mouseGuiX, double mouseGuiY, float poseScale) {
        return FrameChart.paint(surface, x, viewportWidth / 2 - x, bottom,
                mouseGuiX / poseScale, mouseGuiY / poseScale);
    }
}
