package dev.primept.mixin;

import dev.primept.Renderer;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.network.FriendlyByteBuf;
import net.minecraft.world.level.chunk.LevelChunk;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** 捕获已加载chunk真实biome替换；几何事件不再冒充biome缓存变化。 */
@Mixin(LevelChunk.class)
abstract class BiomeSourceMixin {
    @Inject(method="replaceBiomes",at=@At("RETURN"))
    private void primept$biomes(FriendlyByteBuf source,CallbackInfo callback){
        var chunk=(LevelChunk)(Object)this;
        if(chunk.getLevel() instanceof ClientLevel level)Renderer.biomeColumn(level,chunk.getPos().x(),chunk.getPos().z());
    }
}
