package dev.primept;

import java.io.IOException;
import java.io.UncheckedIOException;
import net.minecraft.client.resources.metadata.animation.AnimationMetadataSection;
import net.minecraft.resources.Identifier;
import net.minecraft.server.packs.resources.ResourceManager;

/** 按当前资源包栈取得每张伴随纹理及其自己的动画元数据。像素解释归 Rust。 */
final class LabPbrSource {
    private LabPbrSource() {}

    static Identifier companion(Identifier sprite, String suffix) {
        return Identifier.fromNamespaceAndPath(sprite.getNamespace(), "textures/" + sprite.getPath() + suffix + ".png");
    }

    static void write(Packet packet, ResourceManager resources, Identifier sprite, String suffix) {
        var id = companion(sprite, suffix);
        var selected = resources.getResource(id);
        if (selected.isEmpty()) {
            packet.putInt(0);
            return;
        }
        var resource = selected.get();
        try (var input = resource.open()) {
            packet.putInt(2);
            packet.putStream(input);
            var animation = resource.metadata().getSection(AnimationMetadataSection.TYPE);
            packet.putInt(animation.isPresent() ? 1 : 0);
            if (animation.isPresent()) {
                var a = animation.get();
                packet.putInt(a.frameWidth().orElse(0));
                packet.putInt(a.frameHeight().orElse(0));
                packet.putInt(a.defaultFrameTime());
                packet.putInt(a.interpolatedFrames() ? 1 : 0);
                var frames = a.frames().orElseGet(java.util.List::of);
                packet.putInt(frames.size());
                for (var frame : frames) {
                    packet.putInt(frame.index());
                    packet.putInt(frame.time().orElse(a.defaultFrameTime()));
                }
            }
        } catch (IOException e) {
            throw new UncheckedIOException("读取 " + id + "（" + resource.sourcePackId() + "）", e);
        }
    }
}
