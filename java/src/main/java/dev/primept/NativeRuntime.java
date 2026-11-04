package dev.primept;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.DigestInputStream;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HashSet;
import java.util.HexFormat;
import java.util.Locale;
import java.util.regex.Pattern;
import net.fabricmc.loader.api.FabricLoader;
import static java.lang.foreign.ValueLayout.*;
import static java.nio.file.LinkOption.NOFOLLOW_LINKS;
import static java.nio.file.StandardCopyOption.*;
import static java.nio.file.StandardOpenOption.*;

/** 同一进程的 RR 引导和渲染会话共用一个原生运行时位置。 */
final class NativeRuntime {
    private static final String RESOURCES = "/META-INF/primept/windows-x86_64/";
    private static final String DLL = "prime_pt.dll";
    private static final HexFormat HEX = HexFormat.of();
    private static final Pattern MANIFEST_LINE = Pattern.compile("[0-9a-f]{64}  [A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*");
    private static Location resolved;
    private static SymbolLookup bundledSymbols;

    record Location(Path library, boolean bundled) {}

    private NativeRuntime() {}

    static synchronized Location locate() {
        if (resolved != null) return resolved;
        var override = System.getProperty("primept.native");
        if (override != null) return resolved = new Location(Path.of(override).toAbsolutePath(), false);
        return locateBundled(FabricLoader.getInstance().getGameDir().resolve(".primept/runtime"));
    }

    static synchronized Location locateBundled(Path cache) {
        if (resolved != null) return resolved;
        try {
            return resolved = extract(cache);
        } catch (IOException failure) {
            throw new UncheckedIOException("Prime PT 原生运行时解包失败", failure);
        }
    }

    static SymbolLookup open(Location location, Arena lifetime) {
        return location.bundled() ? openBundle(location.library()) : SymbolLookup.libraryLookup(location.library(), lifetime);
    }

    private static synchronized SymbolLookup openBundle(Path library) {
        if (bundledSymbols != null) return bundledSymbols;
        var directory = library.getParent();
        // Windows 通过模块名解析后续 DLL 的导入；CRT 与发行 DLL 均在进程内保持有效。
        for (var name : new String[] {"vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"})
            SymbolLookup.libraryLookup(directory.resolve(name), Arena.global());
        var symbols = SymbolLookup.libraryLookup(library, Arena.global());
        var linker = Linker.nativeLinker();
        var setRoot = linker.downcallHandle(symbols.find("prime_set_runtime_root").orElseThrow(),
                FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_LONG));
        var root = directory.toString().getBytes(StandardCharsets.UTF_8);
        try (var arena = Arena.ofConfined()) {
            var bytes = arena.allocateFrom(JAVA_BYTE, root);
            if ((int) setRoot.invokeExact(bytes, bytes.byteSize()) != 0) {
                var error = linker.downcallHandle(symbols.find("prime_error").orElseThrow(),
                        FunctionDescriptor.of(ADDRESS));
                var message = (MemorySegment) error.invokeExact();
                throw new IllegalStateException("Prime PT 原生运行时初始化失败："
                        + message.reinterpret(4096).getString(0));
            }
        } catch (RuntimeException | Error failure) {
            throw failure;
        } catch (Throwable failure) {
            throw new IllegalStateException("Prime PT 原生运行时初始化失败", failure);
        }
        return bundledSymbols = symbols;
    }

    /** 每份清单独占内容目录；同清单的并发启动共享已校验文件。 */
    static Location extract(Path cache) throws IOException {
        var os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
        var arch = System.getProperty("os.arch", "").toLowerCase(Locale.ROOT);
        if (!os.startsWith("windows") || !(arch.equals("amd64") || arch.equals("x86_64")))
            throw new IOException("发行包支持 Windows x86-64，当前平台为 " + os + " / " + arch);

        byte[] manifest;
        try (var input = resource("manifest.sha256")) {
            manifest = input.readAllBytes();
        }
        var directory = cache.toAbsolutePath().resolve(HEX.formatHex(digest().digest(manifest)));
        Files.createDirectories(directory);
        var names = new HashSet<String>();
        try (var channel = FileChannel.open(directory.resolve(".lock"), CREATE, WRITE);
             var lock = channel.lock()) {
            for (var line : new String(manifest, StandardCharsets.UTF_8).lines().toList()) {
                if (!MANIFEST_LINE.matcher(line).matches())
                    throw new IOException("原生运行时清单无效");
                var name = line.substring(66);
                var relative = Path.of(name);
                if (relative.startsWith("..") || !relative.normalize().equals(relative)
                        || name.equalsIgnoreCase(".lock") || !names.add(name.toLowerCase(Locale.ROOT)))
                    throw new IOException("原生运行时清单路径无效或重复：" + name);
                extractFile(name, line.substring(0, 64), directory.resolve(relative));
            }
            if (!names.contains(DLL)) throw new IOException("原生运行时清单缺少 " + DLL);
        }
        return new Location(directory.resolve(DLL), true);
    }

    private static void extractFile(String name, String expected, Path output) throws IOException {
        if (Files.isRegularFile(output, NOFOLLOW_LINKS)) {
            try (var input = new DigestInputStream(Files.newInputStream(output), digest())) {
                input.transferTo(OutputStream.nullOutputStream());
                if (expected.equals(HEX.formatHex(input.getMessageDigest().digest()))) return;
            }
        }
        Files.createDirectories(output.getParent());
        var temporary = Files.createTempFile(output.getParent(), ".primept-", ".tmp");
        try (var input = new DigestInputStream(resource(name), digest())) {
            Files.copy(input, temporary, REPLACE_EXISTING);
            if (!expected.equals(HEX.formatHex(input.getMessageDigest().digest())))
                throw new IOException("原生运行时文件校验失败：" + name);
            Files.move(temporary, output, ATOMIC_MOVE, REPLACE_EXISTING);
        } finally {
            Files.deleteIfExists(temporary);
        }
    }

    private static InputStream resource(String name) throws IOException {
        var input = NativeRuntime.class.getResourceAsStream(RESOURCES + name);
        if (input == null) throw new IOException("发行 JAR 缺少原生运行时资源：" + name);
        return input;
    }

    private static MessageDigest digest() {
        try {
            return MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException impossible) {
            throw new AssertionError(impossible);
        }
    }
}
