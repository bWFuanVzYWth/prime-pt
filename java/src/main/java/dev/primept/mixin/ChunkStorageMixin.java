package dev.primept.mixin;

import dev.primept.TerrainSnapshot;
import java.util.concurrent.atomic.AtomicReferenceArray;
import net.minecraft.client.multiplayer.ClientChunkCache;
import net.minecraft.world.level.chunk.LevelChunk;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(targets = "net.minecraft.client.multiplayer.ClientChunkCache$Storage")
abstract class ChunkStorageMixin {
    @Shadow @Final private AtomicReferenceArray<LevelChunk> chunks;

    @Inject(method = "<init>", at = @At("RETURN"))
    private void primept$bind(ClientChunkCache owner, int radius, CallbackInfo callback) {
        ((TerrainSnapshot.Cache) owner).primept$bindChunks(chunks);
    }
}
