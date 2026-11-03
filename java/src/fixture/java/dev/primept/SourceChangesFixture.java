package dev.primept;

/** 通知只保留来源域和顺序；溢出转为全源刷新，之后仍接收新的局部变化。 */
final class SourceChangesFixture {
    public static void main(String[] arguments) {
        var changes = new SourceChanges();
        Object previous = new Object(), current = new Object();
        changes.section(previous, -1, 2, 3);
        changes.all(current);
        var first = changes.poll();
        if (first.world() != previous || first.x() != -1 || first.all())
            throw new AssertionError("旧世界通知必须保留来源供消费者过滤");
        var second = changes.poll();
        if (second.world() != current || !second.all() || changes.poll() != null)
            throw new AssertionError("全源刷新必须保持顺序");
        for (int i = 0; i < 4097; i++) changes.section(current, i, 0, 0);
        if (!changes.takeOverflow() || changes.takeOverflow())
            throw new AssertionError("溢出必须发出一次刷新信号");
        changes.section(current, 8192, -2, -3);
        if (changes.poll().x() != 4096 || changes.poll().x() != 8192 || changes.poll() != null)
            throw new AssertionError("溢出后必须继续接收新变化");
        System.out.println("有界源失效通知通过");
    }
}
