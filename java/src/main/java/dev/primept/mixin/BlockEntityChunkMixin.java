package dev.primept.mixin;

import dev.primept.TerrainSnapshot;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.world.level.chunk.LevelChunk;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(LevelChunk.class)
abstract class BlockEntityChunkMixin {
    @Inject(method = {"setBlockEntity", "removeBlockEntity"}, at = @At("RETURN"))
    private void primept$occupancy(CallbackInfo callback) {
        var chunk = (LevelChunk) (Object) this;
        if (chunk.getLevel() instanceof ClientLevel level)
            ((TerrainSnapshot.Cache) level.getChunkSource()).primept$blockEntitiesChanged(chunk);
    }
}
