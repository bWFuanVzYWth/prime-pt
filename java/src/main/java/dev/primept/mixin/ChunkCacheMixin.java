package dev.primept.mixin;

import dev.primept.TerrainSnapshot;
import java.util.concurrent.atomic.AtomicReferenceArray;
import net.minecraft.client.multiplayer.ClientChunkCache;
import net.minecraft.client.renderer.SectionOcclusionGraph;
import net.minecraft.world.level.chunk.LevelChunk;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Unique;

@Mixin(ClientChunkCache.class)
abstract class ChunkCacheMixin implements TerrainSnapshot.Cache {
    @Unique private AtomicReferenceArray<LevelChunk> primept$chunks;

    public void primept$bindChunks(AtomicReferenceArray<LevelChunk> chunks) { primept$chunks = chunks; }

    public void primept$restoreTerrain(SectionOcclusionGraph graph) {
        TerrainSnapshot.restore((ClientChunkCache) (Object) this, primept$chunks, graph);
    }
}
