package dev.primept;

import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import net.minecraft.client.model.geom.builders.UVPair;
import net.minecraft.client.resources.model.geometry.BakedQuad;
import net.minecraft.core.Direction;
import org.joml.Vector3f;

/** 以真实源 quad 和生产 writer 生成跨 Java/Rust fixture，不初始化游戏或 GPU。 */
final class WireFixture {
    public static void main(String[] arguments) throws Exception {
        var packet = new Packet();
        packet.putInt(0x31545250); packet.putInt(1);
        packet.putInt(-4); packet.putInt(0); packet.putInt(0); packet.putInt(1);
        packet.putInt(1 | 2 << 4 | 3 << 8); packet.putInt(1);
        packet.putFloat(0.25f); packet.putFloat(0.5f); packet.putFloat(-0.125f);
        var source = new BakedQuad(new Vector3f(0.125f, 0, 0.25f), new Vector3f(1, 0, 0.25f),
                new Vector3f(1, 1, 0.25f), new Vector3f(0.125f, 1, 0.25f),
                UVPair.pack(0.25f, 0.5f), UVPair.pack(0.75f, 0.5f),
                UVPair.pack(0.75f, 1), UVPair.pack(0.25f, 1), Direction.NORTH, null);
        packet.quad(source, 0xff80c040);
        var output = Path.of(arguments[0]);
        Files.createDirectories(output.toAbsolutePath().getParent());
        try (var file = FileChannel.open(output, StandardOpenOption.CREATE, StandardOpenOption.TRUNCATE_EXISTING, StandardOpenOption.WRITE)) {
            var bytes = packet.finish();
            while (bytes.hasRemaining()) file.write(bytes);
        }
        System.out.println("Java 源 fixture: " + output.toAbsolutePath());
    }
}
