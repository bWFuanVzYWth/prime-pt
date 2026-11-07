package dev.primept;

import java.util.List;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.components.toasts.SystemToast;
import net.minecraft.client.gui.components.toasts.ToastManager;
import net.minecraft.locale.Language;
import net.minecraft.network.chat.Component;

/** 启动能力失败只保存原因；字体与语言就绪后使用原版常驻通知。 */
public final class SupportNotice {
    private static final SystemToast.SystemToastId TOKEN = new SystemToast.SystemToastId();
    private static Problem problem;
    private static Problem displayed;
    private static Language language;

    record Problem(boolean openGl, List<String> missing) {
        Problem { missing = List.copyOf(missing); }
        Component message() {
            return openGl ? Component.translatable("primept.error.opengl")
                    : Component.translatable("primept.error.capability", missing.getFirst());
        }
    }

    private SupportNotice() {}

    /** 当前宿主不是 Vulkan；不改变用户保存的渲染偏好。 */
    public static void openGl() { report(new Problem(true, List.of())); }

    /** 已确认的缺失项须非空，完整内容只记录一次，通知展示首项避免超过原版通知槽。 */
    static void missing(List<String> requirements) { report(new Problem(false, requirements)); }

    static void supported() { problem = null; }
    static Problem current() { return problem; }

    private static void report(Problem next) {
        if (next.equals(problem)) return;
        problem = next;
        System.getLogger("prime_pt").log(System.Logger.Level.ERROR,
                next.openGl() ? "Prime PT 不可用：当前使用 OpenGL；请切换 Vulkan 并重启。保留原版渲染。"
                        : "Prime PT 不可用，保留原版渲染；缺少：" + String.join(", ", next.missing()));
    }

    /** 原版清空通知队列后重新登记；同一原因不逐帧追加或重播提示。 */
    public static void show() {
        if (problem == null) return;
        var minecraft = Minecraft.getInstance();
        if (!minecraft.isGameLoadFinished()) return;
        var manager = minecraft.gui.toastManager();
        var toast = manager.getToast(PersistentToast.class, TOKEN);
        if (toast == null) {
            manager.addToast(new PersistentToast(problem.message()));
        } else if (!problem.equals(displayed) || language != Language.getInstance()) {
            toast.reset(Component.translatable("primept.error.unavailable"), problem.message());
        }
        displayed = problem;
        language = Language.getInstance();
    }

    private static final class PersistentToast extends SystemToast {
        PersistentToast(Component message) {
            super(TOKEN, Component.translatable("primept.error.unavailable"), message);
        }
        @Override public void update(ToastManager manager, long elapsed) {}
        @Override public Visibility getWantedVisibility() {
            return problem == null ? Visibility.HIDE : Visibility.SHOW;
        }
    }
}
