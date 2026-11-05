package dev.primept;

import com.mojang.brigadier.CommandDispatcher;
import com.mojang.brigadier.StringReader;
import com.mojang.brigadier.builder.LiteralArgumentBuilder;
import com.mojang.brigadier.suggestion.Suggestion;
import com.mojang.brigadier.suggestion.Suggestions;
import java.util.List;
import java.util.Set;

/** 使用真实 Brigadier 验证原版提示消费的数据；不初始化 Minecraft。 */
final class CommandHintsFixture {
    public static void main(String[] arguments) {
        var dispatcher = new CommandDispatcher<Object>();
        CommandHints.install(dispatcher, () -> "-2.5", () -> 3, () -> 9);
        rootPrefix(dispatcher);
        nextKeyword(dispatcher);
        valueCompletion(dispatcher);
        usage(dispatcher);
        serverCommands();
        dispatcherReplacement();
        System.out.println("PT Brigadier suggestion fixture passed");
    }

    private static void rootPrefix(CommandDispatcher<Object> dispatcher) {
        var suggestions = suggestions(dispatcher, "/p");
        require(texts(suggestions).contains("pt"), "斜杠后的半输入根名得到 pt");
        require(suggestions.getList().stream().filter(value -> value.getText().equals("pt"))
                .findFirst().orElseThrow().apply("/p").equals("/pt"), "Tab 替换保留斜杠");
    }

    private static void nextKeyword(CommandDispatcher<Object> dispatcher) {
        require(Set.copyOf(texts(suggestions(dispatcher, "/pt ")))
                .equals(Set.of("enabled", "mode", "renderer", "diagnostic", "mcap", "fresh", "ser", "rr", "quality", "ev", "exposure", "spp", "bounces", "reset")),
                "空格后显示全部下级关键字");
        var suggestions = suggestions(dispatcher, "/pt qual");
        require(texts(suggestions).equals(List.of("quality"))
                && suggestions.getList().getFirst().apply("/pt qual").equals("/pt quality"),
                "下级关键字按前缀过滤且能由 Tab 补全");
    }

    private static void valueCompletion(CommandDispatcher<Object> dispatcher) {
        require(Set.copyOf(texts(suggestions(dispatcher, "/pt rr "))).equals(Set.of("on", "off")),
                "RR 候选只包含有效开关");
        require(Set.copyOf(texts(suggestions(dispatcher, "/pt ser "))).equals(Set.of("on", "off")),
                "SER 候选只包含有效开关");
        require(Set.copyOf(texts(suggestions(dispatcher, "/pt quality "))).equals(Set.of(
                "dlaa", "quality", "balanced", "performance", "ultra_performance")), "列出全部质量档");
        require(texts(suggestions(dispatcher, "/pt quality per")).equals(List.of("performance")),
                "质量档按当前输入过滤");
        require(texts(suggestions(dispatcher, "/pt ev -2")).contains("-2.5")
                && texts(suggestions(dispatcher, "/pt spp ")).contains("3")
                && texts(suggestions(dispatcher, "/pt bounces ")).contains("9"), "数值候选包含当前设置");
    }

    private static void usage(CommandDispatcher<Object> dispatcher) {
        var exposure = dispatcher.getRoot().getChild("pt").getChild("ev");
        require(dispatcher.getSmartUsage(exposure, new Object()).values().stream()
                .anyMatch(value -> value.contains("<EV>")), "原版 usage 可显示下一个 EV 参数");
        require(dispatcher.parse("pt spp 0", new Object()).getReader().canRead(), "无效范围保留语法错误信息");
    }

    private static void serverCommands() {
        var dispatcher = new CommandDispatcher<Object>();
        dispatcher.register(LiteralArgumentBuilder.<Object>literal("server").then(
                LiteralArgumentBuilder.<Object>literal("option").executes(context -> 7)));
        var before = texts(suggestions(dispatcher, "/server "));
        CommandHints.install(dispatcher, () -> "0", () -> 1, () -> 12);
        suggestions(dispatcher, "/pt quality invalid ");
        require(before.equals(texts(suggestions(dispatcher, "/server ")))
                && texts(suggestions(dispatcher, "/ser")).equals(List.of("server")),
                "注入或错误 PT 输入都不改变服务器命令候选");
    }

    private static void dispatcherReplacement() {
        for (int update = 0; update < 2; update++) {
            var dispatcher = new CommandDispatcher<Object>();
            CommandHints.install(dispatcher, () -> "0", () -> 1, () -> 12);
            require(texts(suggestions(dispatcher, "/pt rr ")).size() == 2,
                    "新连接或服务器更新后的 dispatcher 可重新安装本地命令");
        }
    }

    private static Suggestions suggestions(CommandDispatcher<Object> dispatcher, String input) {
        var reader = new StringReader(input);
        if (reader.canRead() && reader.peek() == '/') reader.skip();
        return dispatcher.getCompletionSuggestions(dispatcher.parse(reader, new Object())).join();
    }

    private static List<String> texts(Suggestions suggestions) {
        return suggestions.getList().stream().map(Suggestion::getText).toList();
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
