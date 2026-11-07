package dev.primept;

import dev.primept.mixin.SpriteAccess;
import net.minecraft.client.renderer.texture.SpriteLoader;
import net.minecraft.client.renderer.texture.TextureAtlas;

/** 在原版资源上传时复制 block atlas 第一帧；重载世代替换整个快照。 */
public final class Atlas {
    private static byte[] pixels;
    private static int width, height;
    private static java.util.Collection<net.minecraft.client.renderer.texture.TextureAtlasSprite> sprites = java.util.List.of();
    static long generation;
    private static boolean accepted;
    private Atlas() {}

    /** 原版仍拥有 sprite 图像，本方法不保留其指针。 */
    public static void capture(TextureAtlas atlas, SpriteLoader.Preparations source) {
        if (!Bootstrap.rayTracingEnabled()) return;
        if (!atlas.location().equals(TextureAtlas.LOCATION_BLOCKS)) return;
        int w = source.width(), h = source.height();
        byte[] rgba = new byte[Math.multiplyExact(Math.multiplyExact(w, h), 4)];
        for (var sprite : source.regions().values()) {
            var contents = sprite.contents();
            var image = ((SpriteAccess) contents).primept$image();
            var bytes = image.getPixelBytes();
            int sw = contents.width(), sh = contents.height();
            int frame = SourceAnimation.metadata(contents).firstFrame();
            int sx = frame % (image.getWidth() / sw) * sw, sy = frame / (image.getWidth() / sw) * sh;
            int dx = Math.round(sprite.getU0() * w), dy = Math.round(sprite.getV0() * h);
            for (int row = 0; row < sh; row++)
                bytes.get(((sy + row) * image.getWidth() + sx) * 4, rgba,
                        ((dy + row) * w + dx) * 4, sw * 4);
        }
        width = w; height = h; pixels = rgba;
        sprites = java.util.List.copyOf(source.regions().values()); generation++;
    }

    static boolean ready() { return accepted; }
    static java.util.Collection<net.minecraft.client.renderer.texture.TextureAtlasSprite> sprites() { return sprites; }
    static void send(Native renderer) { renderer.atlas(pixels, width, height); }
    static void accepted() { pixels=null; sprites=java.util.List.of(); accepted=true; }
}
