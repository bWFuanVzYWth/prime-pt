package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Locale;

/** PFT2 的渲染线程显示副本；统计、窗口与保峰抽样由引擎唯一维护。 */
final class FrameChart {
    static final int MAGIC = 0x32544650, VERSION = 3, STAGES = 14, MAX_BARS = 512;
    static final int HEADER_BYTES = 192, DETAIL_BYTES = 128, BAR_BYTES = 120;
    static final int BARS_OFFSET = HEADER_BYTES + 3 * DETAIL_BYTES;
    static final int CAPACITY = BARS_OFFSET + MAX_BARS * BAR_BYTES;
    static final String[] LABELS = {"宿主等待", "tint/源维护", "规划", "宿主捕获", "网格编译/合并", "三角编码", "属性/光源打包",
            "BLAS构建/等待", "属性/光源写入", "TLAS构建/等待", "场景提交/回收", "录制/SDK", "宿主提交", "未归因"};
    private static final String[] SHORT_LABELS = {"等待", "tint", "规划", "捕获", "网格", "三角", "打包",
            "BLAS", "写入", "TLAS", "回收", "录制", "提交", "未归因"};
    static final int[] COLORS = {0xff45b59e, 0xff53c4bd, 0xff9cbbe8, 0xff5b8ff9, 0xffee5b6b,
            0xfff7a8b8, 0xff64cedd, 0xfff0ca52, 0xff90cf66, 0xfff69942, 0xffaa74dd,
            0xff7f8fff, 0xffde91c1, 0xff74808e};
    private static final long[] TOTALS = new long[MAX_BARS], VALUES = new long[MAX_BARS * STAGES];
    private static final long[] DETAIL_TOTALS = new long[3], DETAILS = new long[3 * STAGES];
    private static int bars, detailMask, columns = 240;
    private static long sampleCount, gpuSequence, gpuNs;
    private static double averageFps, meanMs, p90Ms, p99Ms;
    private static boolean gpuAvailable, sceneAvailable;
    private static long sceneSequence, dirtyRegions, instances, triangleBytes, attributeBytes;
    private static long blasSubmitWaitNs, tlasNs, attributeWriteNs;
    private static int pressureKind, excludedRegions;
    private static long requiredBytes, limitBytes, deferredRequests;

    private FrameChart() {}

    /** 供下一帧 native 导出选择保峰桶数；尺寸变化只影响后续快照。 */
    static int columns() { return columns; }
    static boolean ready() { return bars != 0; }

    /** 清除显示副本，不改变下一帧期望宽度；native 的会话重置由调用方维护。 */
    static void clear() {
        bars = 0; detailMask = 0; sampleCount = 0;
        averageFps = 0; meanMs = 0; p90Ms = 0; p99Ms = 0;
        gpuAvailable = false; gpuSequence = 0; gpuNs = 0;
        sceneAvailable = false; sceneSequence = 0; dirtyRegions = 0; instances = 0;
        triangleBytes = 0; attributeBytes = 0; blasSubmitWaitNs = 0; tlasNs = 0; attributeWriteNs = 0;
        pressureKind = 0; excludedRegions = 0; requiredBytes = 0; limitBytes = 0; deferredRequests = 0;
    }

    /**
     * 读取 little-endian PFT2，不保留传入缓冲。完整校验后才发布显示副本；
     * 非法布局、负时长、归因和不等于整帧或非有限统计抛 IllegalArgumentException。
     * 隐藏时此入口只更新复用数组和数值；文本格式化留到可见的 paint。
     * header 0/4/8/12 为 magic/version/stages/bars，16/24 为样本数/序号，
     * 32..56 为 FPS/mean/P90/P99 的 f64，64 为 GPU valid，72/80 为序号/ns，88 为 detail mask。
     * 92 为 scene valid，96..152 为场景序号、dirty/instance 数、三角/属性字节与 BLAS提交等待/TLAS/属性写入纳秒。
     * 场景字段属于最近一次更新，不与选中帧绑定。v3在160/164保存CPU压力原因/排除region数，
     * 168/176/184保存需要字节/预算字节/延后请求数；压力属于当前状态。
     * 192 起三条 128B detail（total/reserved/14段），576 起 120B bar（total/14段）。
     */
    static void update(ByteBuffer source) {
        if (source.order() != ByteOrder.LITTLE_ENDIAN || source.limit() < BARS_OFFSET
                || source.getInt(0) != MAGIC || source.getInt(4) != VERSION || source.getInt(8) != STAGES)
            throw invalid();
        int count = source.getInt(12), mask = source.getInt(88), valid = source.getInt(64), scene = source.getInt(92);
        if (count < 0 || count > MAX_BARS || source.limit() < BARS_OFFSET + count * BAR_BYTES
                || source.getLong(16) < count || source.getLong(24) < 0 || (mask & ~7) != 0
                || valid < 0 || valid > 1 || scene < 0 || scene > 1 || source.getInt(68) != 0)
            throw invalid();
        if (source.getInt(160) < 0 || source.getInt(160) > 3 || source.getInt(164) < 0
                || source.getLong(168) < 0 || source.getLong(176) < 0 || source.getLong(184) < 0) throw invalid();
        for (int offset = 32; offset <= 56; offset += 8)
            if (!Double.isFinite(source.getDouble(offset)) || source.getDouble(offset) < 0) throw invalid();
        if (valid != 0 && (source.getLong(72) < 0 || source.getLong(80) < 0)) throw invalid();
        if (scene != 0) for (int offset = 96; offset <= 152; offset += 8)
            if (source.getLong(offset) < 0) throw invalid();
        for (int detail = 0; detail < 3; detail++) {
            int offset = HEADER_BYTES + detail * DETAIL_BYTES;
            if (source.getLong(offset + 8) != 0) throw invalid();
            if ((mask & (1 << detail)) != 0) validateSample(source, offset, offset + 16);
        }
        for (int bar = 0; bar < count; bar++) {
            int offset = BARS_OFFSET + bar * BAR_BYTES;
            validateSample(source, offset, offset + 8);
        }
        for (int detail = 0; detail < 3; detail++) {
            int offset = HEADER_BYTES + detail * DETAIL_BYTES;
            DETAIL_TOTALS[detail] = source.getLong(offset);
            for (int stage = 0; stage < STAGES; stage++)
                DETAILS[detail * STAGES + stage] = source.getLong(offset + 16 + stage * 8);
        }
        for (int bar = 0; bar < count; bar++) {
            int offset = BARS_OFFSET + bar * BAR_BYTES;
            TOTALS[bar] = source.getLong(offset);
            for (int stage = 0; stage < STAGES; stage++)
                VALUES[bar * STAGES + stage] = source.getLong(offset + 8 + stage * 8);
        }
        sampleCount = source.getLong(16); detailMask = mask; bars = count;
        averageFps = source.getDouble(32); meanMs = source.getDouble(40);
        p90Ms = source.getDouble(48); p99Ms = source.getDouble(56);
        gpuAvailable = valid != 0; gpuSequence = source.getLong(72); gpuNs = source.getLong(80);
        sceneAvailable = scene != 0; sceneSequence = source.getLong(96);
        dirtyRegions = source.getLong(104); instances = source.getLong(112);
        triangleBytes = source.getLong(120); attributeBytes = source.getLong(128);
        blasSubmitWaitNs = source.getLong(136); tlasNs = source.getLong(144); attributeWriteNs = source.getLong(152);
        pressureKind = source.getInt(160); excludedRegions = source.getInt(164);
        requiredBytes = source.getLong(168); limitBytes = source.getLong(176); deferredRequests = source.getLong(184);
    }

    private static IllegalArgumentException invalid() { return new IllegalArgumentException("无效 PFT2 帧图快照"); }
    private static void validateSample(ByteBuffer source, int totalOffset, int valuesOffset) {
        long total = source.getLong(totalOffset), remaining = total;
        if (total <= 0) throw invalid();
        for (int stage = 0; stage < STAGES; stage++) {
            long value = source.getLong(valuesOffset + stage * 8);
            if (value < 0 || value > remaining) throw invalid();
            remaining -= value;
        }
        if (remaining != 0) throw invalid();
    }

    /** Minecraft 与离屏 fixture 共用的绘制接口，坐标为 GUI 逻辑像素。 */
    interface Surface {
        void fill(int x0, int y0, int x1, int y1, int color);
        void text(String text, int x, int y, int color);
        int textWidth(String text);
    }

    /** 仅绘制快照；不读取 GPU、等待、排序或重新抽样。右半屏留给宿主 TPS 图。 */
    static boolean paint(Surface surface, int x, int width, int height, double mouseX, double mouseY) {
        if (width < 96 || height < 120) return false;
        width = Math.min(width, MAX_BARS + 12);
        int graphWidth = width - 12;
        columns = graphWidth;
        boolean compact = height < 260 || graphWidth < 240;
        int legendColumns = Math.max(1, Math.min(5, graphWidth / (compact ? 36 : 100)));
        int legendRows = (STAGES + legendColumns - 1) / legendColumns;
        int detailRows = compact ? 2 : 3;
        int pressureHeight = pressureKind == 0 && excludedRegions == 0 && deferredRequests == 0 ? 0 : 22;
        int fixedHeight = 51 + detailRows * 11 + 6 + legendRows * 10 + 8 + 44 + pressureHeight + 6;
        if (height < fixedHeight + 20) return false;
        int graphHeight = Math.min(72, height - fixedHeight);
        int panelHeight = fixedHeight + graphHeight;
        int y = height - panelHeight;
        surface.fill(x, y, x + width, height, 0xdd10141c);
        text(surface, "Prime PT · 最近1秒 · " + sampleCount + "帧", x + 6, y + 4, graphWidth, 0xffffffff);
        text(surface, format("%.1f FPS  均值 %.2f ms", averageFps, meanMs), x + 6, y + 15, graphWidth, 0xffeeeeee);
        text(surface, format("P90 %.2f  P99 %.2f ms", p90Ms, p99Ms), x + 6, y + 26, graphWidth, 0xffeeeeee);
        String gpu = gpuAvailable ? format("GPU已完成 #%d %.2fms", gpuSequence, gpuNs / 1_000_000.0) : "GPU 时间未采集";
        text(surface, "主线程墙钟，含等待 · " + gpu, x + 6, y + 37, graphWidth, 0xffaab7c9);
        int left = x + 6, top = y + 51, bottom = top + graphHeight;
        long maximum = 33_333_333;
        int first = Math.max(0, bars - graphWidth), visible = bars - first;
        for (int bar = first; bar < bars; bar++) maximum = Math.max(maximum, TOTALS[bar]);
        surface.fill(left, top, left + graphWidth, bottom, 0xff070a10);
        int reference = bottom - (int) (16_666_667.0 / maximum * graphHeight);
        surface.fill(left, reference, left + graphWidth, reference + 1, 0xff4b5260);
        for (int index = 0; index < visible; index++) {
            int bar = first + index;
            int x0 = left + index * graphWidth / visible, x1 = left + (index + 1) * graphWidth / visible;
            long cumulative = 0;
            int previousY = bottom;
            for (int stage = 0; stage < STAGES; stage++) {
                cumulative += VALUES[bar * STAGES + stage];
                int currentY = bottom - (int) Math.round((double) cumulative / maximum * graphHeight);
                if (currentY < previousY) surface.fill(x0, currentY, x1, previousY, COLORS[stage]);
                previousY = currentY;
            }
        }
        int hovered = -1;
        if (visible != 0 && mouseX >= left && mouseX < left + graphWidth && mouseY >= top && mouseY < bottom) {
            int index = Math.min(visible - 1, (int) ((mouseX - left) * visible / graphWidth));
            hovered = first + index;
            int x0 = left + index * graphWidth / visible;
            surface.fill(x0, top, x0 + 1, bottom, 0xffffffff);
        }
        int selected = (detailMask & 2) != 0 ? 1 : 0;
        long total = hovered >= 0 ? TOTALS[hovered] : DETAIL_TOTALS[selected];
        long[] values = hovered >= 0 ? VALUES : DETAILS;
        int offset = (hovered >= 0 ? hovered : selected) * STAGES;
        boolean selectedAvailable = hovered >= 0 || (detailMask & (1 << selected)) != 0;
        String prefix = hovered >= 0 ? "选中" : selected == 1 ? "最近卡顿" : "最新帧";
        text(surface, selectedAvailable ? format("%s %.2fms · 每柱保留最长帧", prefix, total / 1_000_000.0) : "等待有效样本",
                left, bottom + 4, graphWidth, 0xffffffff);
        if (selectedAvailable) {
            int largest = largestStage(values, offset);
            String label = compact ? SHORT_LABELS[largest] : LABELS[largest];
            text(surface, format("%s%s %.1f%%  %.2fms", compact ? "最大 " : "最大项：", label,
                    values[offset + largest] * 100.0 / total, values[offset + largest] / 1_000_000.0),
                    left, bottom + 15, graphWidth, COLORS[largest]);
        }
        if (!compact) {
            String slowest = "会话最慢 --";
            if ((detailMask & 4) != 0) {
                int largest = largestStage(DETAILS, 2 * STAGES);
                slowest = format("会话最慢 %.2fms · %s %.1f%%", DETAIL_TOTALS[2] / 1_000_000.0,
                        LABELS[largest], DETAILS[2 * STAGES + largest] * 100.0 / DETAIL_TOTALS[2]);
            }
            text(surface, slowest, left, bottom + 26, graphWidth, 0xffc2cad6);
        }
        int legendTop = bottom + detailRows * 11 + 6;
        int cellWidth = graphWidth / legendColumns;
        for (int stage = 0; stage < STAGES; stage++) {
            int lx = left + stage % legendColumns * cellWidth, ly = legendTop + stage / legendColumns * 10;
            surface.fill(lx, ly + 1, lx + 5, ly + 6, COLORS[stage]);
            text(surface, compact ? SHORT_LABELS[stage] : LABELS[stage], lx + 8, ly, cellWidth - 9, 0xffd5dbe4);
        }
        int sceneTop = legendTop + legendRows * 10 + 8;
        surface.fill(left, sceneTop - 3, left + graphWidth, sceneTop - 2, 0xff465262);
        text(surface, sceneAvailable ? "最近场景更新 #" + sceneSequence : "最近场景更新：尚无", left, sceneTop, graphWidth, 0xffc2cad6);
        if (sceneAvailable) {
            text(surface, format("几何脏区%d · 实例%d · 三角%s", dirtyRegions, instances, bytes(triangleBytes)),
                    left, sceneTop + 11, graphWidth, 0xffc2cad6);
            text(surface, format("场景写入%s · %.2fms", bytes(attributeBytes), attributeWriteNs / 1_000_000.0),
                    left, sceneTop + 22, graphWidth, COLORS[8]);
            text(surface, compact ? format("BLAS等 %.1f · TLAS %.1f ms", blasSubmitWaitNs / 1_000_000.0, tlasNs / 1_000_000.0)
                    : format("BLAS提交/等待 %.2fms · TLAS %.2fms", blasSubmitWaitNs / 1_000_000.0, tlasNs / 1_000_000.0),
                    left, sceneTop + 33, graphWidth, 0xffc2cad6);
        }
        if (pressureHeight != 0) {
            String cause = switch (pressureKind) { case 1 -> "常驻预算"; case 2 -> "等待回收"; case 3 -> "单区预算"; default -> "容量状态"; };
            text(surface, format("CPU %s：%s / %s", cause, bytes(requiredBytes), bytes(limitBytes)), left, sceneTop + 44, graphWidth, 0xffffc46b);
            text(surface, format("预算排除%d区 · 延后%d请求", excludedRegions, deferredRequests), left, sceneTop + 55, graphWidth, 0xffffc46b);
        }
        return true;
    }

    private static int largestStage(long[] values, int offset) {
        int largest = 0;
        for (int stage = 1; stage < STAGES; stage++) if (values[offset + stage] > values[offset + largest]) largest = stage;
        return largest;
    }
    private static String bytes(long value) {
        if (value >= 1_048_576) return format("%.2fMiB", value / 1_048_576.0);
        if (value >= 1024) return format("%.1fKiB", value / 1024.0);
        return value + "B";
    }
    private static String format(String format, Object... arguments) { return String.format(Locale.ROOT, format, arguments); }
    private static void text(Surface surface, String value, int x, int y, int width, int color) {
        if (surface.textWidth(value) > width) {
            int end = value.length();
            while (end > 0 && surface.textWidth(value.substring(0, end) + "…") > width) end--;
            value = value.substring(0, end) + "…";
        }
        surface.text(value, x, y, color);
    }
}
