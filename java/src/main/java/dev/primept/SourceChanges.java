package dev.primept;

import java.util.ArrayDeque;

/** 跨宿主回调的有界失效通知；不拥有已加载窗口或捕获结果。 */
final class SourceChanges {
    record Change(Object world, int x, int y, int z, boolean all) {}
    private static final int CAPACITY = 4096;
    private final ArrayDeque<Change> pending = new ArrayDeque<>();
    private boolean overflow;

    synchronized void section(Object world, int x, int y, int z) {
        add(new Change(world, x, y, z, false));
    }

    synchronized void all(Object world) { add(new Change(world, 0, 0, 0, true)); }

    private void add(Change change) {
        if (pending.size() == CAPACITY) {
            pending.clear();
            overflow = true;
        }
        pending.addLast(change);
    }

    synchronized boolean takeOverflow() {
        boolean result = overflow;
        overflow = false;
        return result;
    }

    synchronized Change poll() { return pending.pollFirst(); }
}
