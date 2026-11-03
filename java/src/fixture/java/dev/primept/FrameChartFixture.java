package dev.primept;

import java.awt.Color;
import java.awt.Font;
import java.awt.RenderingHints;
import java.awt.image.BufferedImage;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import javax.imageio.ImageIO;

/** PFT2 边界与生产同源绘制的无窗口验收；不创建 Minecraft 或 Vulkan 对象。 */
final class FrameChartFixture {
    public static void main(String[] arguments) throws Exception {
        Path image = null, nativeFile = null;
        for (var argument : arguments) {
            if (argument.startsWith("--native=")) nativeFile = Path.of(argument.substring(9));
            else image = Path.of(argument);
        }
        if (nativeFile != null) {
            var bytes = ByteBuffer.wrap(Files.readAllBytes(nativeFile)).order(ByteOrder.LITTLE_ENDIAN);
            FrameChart.update(bytes);
            var nativeBounds = new Bounds(520, 360);
            require(FrameChart.ready() && FrameChart.paint(nativeBounds, 0, 520, 360, -1, -1), "接受真实 native producer 输出");
            require(nativeBounds.labels.stream().anyMatch(line -> line.contains("最近卡顿 80.00ms"))
                    && nativeBounds.labels.stream().anyMatch(line -> line.contains("BLAS构建/等待")
                    && line.contains("81.3%") && line.contains("65.00ms")), "native 峰值、最大项与占比跨语言一致");
            require(nativeBounds.labels.stream().anyMatch(line -> line.contains("最近场景更新 #2"))
                    && nativeBounds.labels.stream().anyMatch(line -> line.contains("脏区2") && line.contains("实例64") && line.contains("6.00MiB"))
                    && nativeBounds.labels.stream().anyMatch(line -> line.contains("场景写入3.00MiB") && line.contains("1.00ms"))
                    && nativeBounds.labels.stream().anyMatch(line -> line.contains("BLAS提交/等待 1.00ms") && line.contains("TLAS 2.00ms")),
                    "真实场景规模与主机子段独立显示");
            if (image != null) render(image.resolveSibling(image.getFileName().toString().replace(".png", "-native.png")));
            System.out.println("Native PFT2 -> Java frame chart producer/consumer fixture passed");
        }
        var packet = snapshot(240);
        FrameChart.clear();
        require(!FrameChart.ready(), "clear 隐藏旧世界快照");
        FrameChart.update(packet);
        require(FrameChart.ready(), "接受有效快照");
        var sink = new Bounds(520, 360);
        require(FrameChart.paint(sink, 0, 520, 360, -1, -1), "标准 GUI 可绘制");
        require(sink.labels.stream().anyMatch(line -> line.contains("主线程墙钟，含等待")), "明确 CPU 时钟域");
        require(sink.labels.stream().anyMatch(line -> line.contains("GPU 时间未采集")), "无查询不伪造 GPU 时间");
        require(sink.labels.stream().anyMatch(line -> line.contains("最近卡顿 80.00ms"))
                && sink.labels.stream().anyMatch(line -> line.contains("BLAS构建/等待") && line.contains("81.3%") && line.contains("65.00ms")),
                "卡顿直接显示最大项及整帧占比");
        require(sink.labels.stream().anyMatch(line -> line.contains("最近场景更新 #302")), "场景输入明确标记独立更新序号");
        require(FrameChart.columns() == 508, "下一次 native 保峰使用实际图宽");
        var hover = new Bounds(520, 360);
        FrameChart.paint(hover, 0, 520, 360, 177, 200);
        require(hover.labels.stream().anyMatch(line -> line.contains("选中 80.00ms"))
                && hover.labels.stream().anyMatch(line -> line.contains("BLAS构建/等待") && line.contains("75.0%") && line.contains("60.00ms")),
                "悬停展示该柱的完整真实样本归因");
        var narrow = new Bounds(160, 200);
        require(FrameChart.paint(narrow, 0, 160, 200, -1, -1), "小窗口有界");
        require(narrow.labels.stream().anyMatch(line -> line.contains("BLAS 81.3%") && line.contains("65.00ms")),
                "窄GUI仍直接显示最大项及占比");
        require(!FrameChart.paint(new Bounds(96, 120), 0, 96, 120, -1, -1), "过小窗口回到宿主图");
        rejects(packet, 0, 0x31544650, "旧 PFT1 不混读");
        rejects(packet, 8, 10, "阶段数量");
        rejects(packet, 92, 2, "场景有效标志");
        packet.putLong(128, -1); expectRejected(packet, "负属性字节"); packet.putLong(128, 3 * 1_048_576L);
        rejects(packet, 160, 4, "CPU压力原因枚举");
        packet.putInt(160, 1); packet.putInt(164, 3); packet.putLong(168, 150 * 1_048_576L); packet.putLong(176, 128 * 1_048_576L); packet.putLong(184, 7);
        FrameChart.update(packet);
        var pressure = new Bounds(520, 360); FrameChart.paint(pressure, 0, 520, 360, -1, -1);
        require(pressure.labels.stream().anyMatch(line -> line.contains("常驻预算") && line.contains("150.00MiB"))
                && pressure.labels.stream().anyMatch(line -> line.contains("预算排除3区") && line.contains("延后7请求")), "预算压力明确显示而非异常");
        packet.putLong(160, 0); packet.putLong(168, 0); packet.putLong(176, 0); packet.putLong(184, 0); FrameChart.update(packet);
        rejects(packet, 12, FrameChart.MAX_BARS + 1, "柱数上界");
        packet.putLong(FrameChart.BARS_OFFSET + 8, -1);
        expectRejected(packet, "负归因");
        packet = snapshot(240);
        packet.putLong(FrameChart.BARS_OFFSET, 1);
        expectRejected(packet, "归因和必须等于整帧");
        packet = snapshot(240);
        packet.putDouble(32, Double.NaN);
        expectRejected(packet, "统计必须有限");
        var preserved = new Bounds(520, 360);
        FrameChart.paint(preserved, 0, 520, 360, -1, -1);
        require(FrameChart.ready() && preserved.labels.stream().anyMatch(line -> line.contains("最近卡顿 80.00ms"))
                && preserved.labels.stream().anyMatch(line -> line.contains("BLAS构建/等待") && line.contains("65.00ms")),
                "非法输入保留上一已校验副本");
        packet = snapshot(240); FrameChart.update(packet);
        if (image != null) render(image);
        FrameChart.clear(); require(!FrameChart.ready(), "关闭后不显示旧数据");
        System.out.println("Frame chart PFT2/layout/offscreen fixture passed");
    }

    private static ByteBuffer snapshot(int count) {
        var data = ByteBuffer.allocate(FrameChart.CAPACITY).order(ByteOrder.LITTLE_ENDIAN);
        data.putInt(0, FrameChart.MAGIC); data.putInt(4, FrameChart.VERSION);
        data.putInt(8, FrameChart.STAGES); data.putInt(12, count);
        data.putLong(16, 300); data.putLong(24, 301);
        data.putDouble(32, 118.4); data.putDouble(40, 8.45); data.putDouble(48, 9.2); data.putDouble(56, 22.5);
        data.putInt(88, 7);
        data.putInt(92, 1); data.putLong(96, 302); data.putLong(104, 2); data.putLong(112, 64);
        data.putLong(120, 6 * 1_048_576L); data.putLong(128, 3 * 1_048_576L);
        data.putLong(136, 1_000_000); data.putLong(144, 2_000_000); data.putLong(152, 1_000_000);
        sample(data, FrameChart.HEADER_BYTES, 16, 8_000_000, 11, 3_000_000);
        sample(data, FrameChart.HEADER_BYTES + FrameChart.DETAIL_BYTES, 16, 80_000_000, 7, 65_000_000);
        sample(data, FrameChart.HEADER_BYTES + 2 * FrameChart.DETAIL_BYTES, 16, 130_000_000, 3, 110_000_000);
        for (int bar = 0; bar < count; bar++) {
            long total = 7_000_000 + bar % 11 * 300_000;
            int stage = bar < 60 ? 11 : bar < 120 ? 3 : bar < 180 ? 7 : 0;
            if (bar == 80) { total = 80_000_000; stage = 7; }
            int offset = FrameChart.BARS_OFFSET + bar * FrameChart.BAR_BYTES;
            sample(data, offset, 8, total, stage, total * 3 / 4);
            if (bar == 80) {
                data.putLong(offset + 8 + 8 * 8, 8_000_000);
                data.putLong(offset + 8 + 9 * 8, 4_000_000);
                data.putLong(offset + 8 + 13 * 8, 8_000_000);
            }
        }
        return data;
    }
    private static void sample(ByteBuffer data, int offset, int valuesOffset, long total, int stage, long value) {
        data.putLong(offset, total); data.putLong(offset + valuesOffset + stage * 8, value);
        data.putLong(offset + valuesOffset + (FrameChart.STAGES - 1) * 8, total - value);
    }
    private static void rejects(ByteBuffer data, int offset, int value, String reason) {
        int previous = data.getInt(offset); data.putInt(offset, value);
        expectRejected(data, reason); data.putInt(offset, previous);
    }
    private static void expectRejected(ByteBuffer data, String reason) {
        try { FrameChart.update(data); throw new AssertionError(reason); }
        catch (IllegalArgumentException expected) { /* 只接受明确协议错误。 */ }
    }

    private static final class Bounds implements FrameChart.Surface {
        final int width, height;
        final ArrayList<String> labels = new ArrayList<>();
        Bounds(int width, int height) { this.width = width; this.height = height; }
        public void fill(int x0, int y0, int x1, int y1, int color) {
            require(x0 >= 0 && x1 >= x0 && x1 <= width && y0 >= 0 && y1 >= y0 && y1 <= height, "矩形有界");
        }
        public void text(String text, int x, int y, int color) {
            require(x >= 0 && x + textWidth(text) <= width && y >= 0 && y + 9 <= height, "文字有界");
            labels.add(text);
        }
        public int textWidth(String text) { return text.length() * 5; }
    }

    private static void render(Path output) throws Exception {
        renderImage(output, 520, 360);
        renderImage(output.resolveSibling(output.getFileName().toString().replace(".png", "-compact.png")), 160, 200);
    }
    private static void renderImage(Path output, int width, int height) throws Exception {
        Files.createDirectories(output.toAbsolutePath().getParent());
        var image = new BufferedImage(width * 2, height * 2, BufferedImage.TYPE_INT_ARGB);
        var graphics = image.createGraphics();
        try {
            graphics.setColor(new Color(0xff29384b, true)); graphics.fillRect(0, 0, width * 2, height * 2);
            graphics.scale(2, 2);
            graphics.setFont(new Font("Microsoft YaHei", Font.PLAIN, 9));
            graphics.setRenderingHint(RenderingHints.KEY_TEXT_ANTIALIASING, RenderingHints.VALUE_TEXT_ANTIALIAS_ON);
            FrameChart.paint(new FrameChart.Surface() {
                public void fill(int x0, int y0, int x1, int y1, int color) {
                    graphics.setColor(new Color(color, true)); graphics.fillRect(x0, y0, x1 - x0, y1 - y0);
                }
                public void text(String text, int x, int y, int color) {
                    graphics.setColor(new Color(color, true)); graphics.drawString(text, x, y + 8);
                }
                public int textWidth(String text) { return graphics.getFontMetrics().stringWidth(text); }
            }, 0, width, height, -1, -1);
        } finally { graphics.dispose(); }
        ImageIO.write(image, "png", output.toFile());
    }
    private static void require(boolean value, String message) { if (!value) throw new AssertionError(message); }
}
