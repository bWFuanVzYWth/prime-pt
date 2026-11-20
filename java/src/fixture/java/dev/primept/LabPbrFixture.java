package dev.primept;

import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.channels.FileChannel;
import java.nio.file.StandardOpenOption;
import java.util.List;
import java.util.Optional;
import net.minecraft.SharedConstants;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;
import net.minecraft.server.packs.PackLocationInfo;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.PathPackResources;
import net.minecraft.server.packs.VanillaPackResourcesBuilder;
import net.minecraft.server.packs.repository.PackSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;

/** 从实际资源包栈写出生产资源协议，供 Rust 验证材质图集。 */
final class LabPbrFixture {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        var names = List.of("copper_block", "diamond_block", "stone", "sculk", "glowstone");
        var packet = new Packet();
        packet.putInt(0x314d5250);
        packet.putInt(263);
        packet.putLong(1);
        packet.putInt(1);
        for (int i = 0; i < 4; i++) packet.putInt(0);
        packet.putInt(names.size());
        try (var zip = FileSystems.newFileSystem(Path.of(args[0]));
             var resources = new MultiPackResourceManager(PackType.CLIENT_RESOURCES, List.of(
                     new VanillaPackResourcesBuilder().pushJarResources().exposeNamespace("minecraft")
                             .build(location("vanilla")).fullResources(),
                     new PathPackResources(location("reference"), zip.getPath("/"))))) {
            for (int i = 0; i < names.size(); i++) {
                var sprite = Identifier.fromNamespaceAndPath("minecraft", "block/" + names.get(i));
                packet.putInt(i + 1);
                packet.putString(sprite.toString());
                packet.putFloat((float)i / names.size()); packet.putFloat(0);
                packet.putFloat((float)(i + 1) / names.size()); packet.putFloat(1);
                packet.putInt(16); packet.putInt(16);
                packet.putInt(1); packet.putInt(0); packet.putInt(1);
                packet.putInt(16); packet.putInt(16); packet.putInt(0);
                LabPbrSource.write(packet, resources, sprite, "_n");
                LabPbrSource.write(packet, resources, sprite, "_s");
            }
        }
        var output = Path.of(args[1]);
        Files.createDirectories(output.getParent());
        try (var channel = FileChannel.open(output, StandardOpenOption.CREATE,
                StandardOpenOption.TRUNCATE_EXISTING, StandardOpenOption.WRITE)) {
            var bytes = packet.finish();
            while (bytes.hasRemaining()) channel.write(bytes);
        }
    }

    private static PackLocationInfo location(String name) {
        return new PackLocationInfo(name, Component.literal(name), PackSource.DEFAULT, Optional.empty());
    }
}
