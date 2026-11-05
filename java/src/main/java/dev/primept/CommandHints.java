package dev.primept;

import com.mojang.brigadier.CommandDispatcher;
import com.mojang.brigadier.arguments.FloatArgumentType;
import com.mojang.brigadier.arguments.IntegerArgumentType;
import com.mojang.brigadier.builder.LiteralArgumentBuilder;
import com.mojang.brigadier.builder.RequiredArgumentBuilder;
import com.mojang.brigadier.suggestion.Suggestions;
import com.mojang.brigadier.suggestion.SuggestionsBuilder;
import java.util.LinkedHashSet;
import java.util.Locale;
import java.util.concurrent.CompletableFuture;
import java.util.function.IntSupplier;
import java.util.function.Supplier;

/** 将本地命令接入原版 Brigadier 提示；执行由 Settings.command 在发送前接管。 */
public final class CommandHints {
    private CommandHints() {}

    public static <S> void install(CommandDispatcher<S> dispatcher) {
        install(dispatcher, () -> number(Settings.exposure()), Settings::samples, Settings::bounces);
    }

    static <S> void install(CommandDispatcher<S> dispatcher, Supplier<String> exposure,
            IntSupplier samples, IntSupplier bounces) {
        var root = CommandHints.<S>command("pt");
        root.then(CommandHints.<S>command("enabled").then(command("on")).then(command("off")));
        var mode = CommandHints.<S>command("mode");
        for (var value : RenderMode.values()) mode.then(command(value.key));
        root.then(mode);
        var renderer = CommandHints.<S>command("renderer");
        for (var value : RealtimeRenderer.values()) renderer.then(command(value.key));
        root.then(renderer);
        root.then(CommandHints.<S>command("diagnostic").then(command("on")).then(command("off")));
        root.then(CommandHints.<S>command("cv").then(command("on")).then(command("off")));
        root.then(CommandHints.<S>command("mcap").then(RequiredArgumentBuilder.<S, Integer>argument("历史M上限",
                IntegerArgumentType.integer(1, 100)).executes(context -> 0)));
        root.then(CommandHints.<S>command("fresh").then(RequiredArgumentBuilder.<S, Integer>argument("初采输出百分比",
                IntegerArgumentType.integer(0, 100)).executes(context -> 0)));
        root.then(CommandHints.<S>command("rr").then(command("on")).then(command("off")));
        root.then(CommandHints.<S>command("ser").then(command("on")).then(command("off")));
        var quality = CommandHints.<S>command("quality");
        for (var value : Quality.values()) quality.then(command(value.key()));
        root.then(quality);
        for (var key : new String[] {"ev", "exposure"}) {
            root.then(CommandHints.<S>command(key).then(RequiredArgumentBuilder.<S, Float>argument("EV",
                            FloatArgumentType.floatArg(SettingsConfig.MIN_EV, SettingsConfig.MAX_EV))
                    .suggests((context, builder) -> suggest(builder, exposure.get(), "0", "-2", "-1", "1", "2"))
                    .executes(context -> 0)));
        }
        root.then(CommandHints.<S>command("spp").then(RequiredArgumentBuilder.<S, Integer>argument("样本数",
                        IntegerArgumentType.integer(SettingsConfig.MIN_SAMPLES, SettingsConfig.MAX_SAMPLES))
                .suggests((context, builder) -> suggest(builder, Integer.toString(samples.getAsInt()), "1", "2", "4", "8", "16", "32", "64"))
                .executes(context -> 0)));
        root.then(CommandHints.<S>command("bounces").then(RequiredArgumentBuilder.<S, Integer>argument("顶点数",
                        IntegerArgumentType.integer(SettingsConfig.MIN_BOUNCES, SettingsConfig.MAX_BOUNCES))
                .suggests((context, builder) -> suggest(builder, Integer.toString(bounces.getAsInt()), "12", "1", "4", "8", "16", "32", "64"))
                .executes(context -> 0)));
        root.then(command("reset"));
        dispatcher.register(root);
    }

    private static <S> LiteralArgumentBuilder<S> command(String keyword) {
        return LiteralArgumentBuilder.<S>literal(keyword).executes(context -> 0);
    }

    private static CompletableFuture<Suggestions> suggest(SuggestionsBuilder builder, String... candidates) {
        String prefix = builder.getRemainingLowerCase();
        for (var candidate : new LinkedHashSet<>(java.util.List.of(candidates)))
            if (candidate.toLowerCase(Locale.ROOT).startsWith(prefix)) builder.suggest(candidate);
        return builder.buildFuture();
    }

    private static String number(float value) {
        return value == (int) value ? Integer.toString((int) value) : Float.toString(value);
    }
}
