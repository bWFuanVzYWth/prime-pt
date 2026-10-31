package dev.primept.mixin;

import com.mojang.blaze3d.platform.NativeImage;
import net.minecraft.client.renderer.texture.SpriteContents;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;

/** 资源上传期间借用原版 CPU 像素。 */
@Mixin(SpriteContents.class)
public interface SpriteAccess {
    /** 生命周期仍归 SpriteContents，调用方只做同步复制。 */
    @Accessor("originalImage") NativeImage primept$image();
}
