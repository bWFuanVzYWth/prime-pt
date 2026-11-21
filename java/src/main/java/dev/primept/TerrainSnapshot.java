package dev.primept;

import it.unimi.dsi.fastutil.longs.LongOpenHashSet;
import java.util.concurrent.atomic.AtomicReferenceArray;
import net.minecraft.client.Camera;
import net.minecraft.client.multiplayer.ClientChunkCache;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.LevelRenderer;
import net.minecraft.client.renderer.SectionOcclusionGraph;
import net.minecraft.client.renderer.state.level.LevelRenderState;
import net.minecraft.core.SectionPos;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;

/** 原版重新取得世界绘制权时，从当前缓存恢复已被消费的加载通知。 */
public final class TerrainSnapshot {
    private TerrainSnapshot() {}

    /** 数组归当前 ClientChunkCache 所有，Storage 换代时立即替换借用。 */
    public interface Cache {
        void primept$blockEntities(ClientLevel level, LevelRenderer renderer, Camera camera,
                float partial, LevelRenderState state);
        void primept$releaseBlockEntities();
        void primept$blockEntitiesChanged(LevelChunk chunk);
        void primept$bindChunks(AtomicReferenceArray<LevelChunk> chunks);
        void primept$restoreTerrain(SectionOcclusionGraph graph);
    }

    public static void restore(ClientChunkCache cache, AtomicReferenceArray<LevelChunk> chunks,
            SectionOcclusionGraph graph) {
        var loaded = new LongOpenHashSet();
        var empty = new LongOpenHashSet();
        var none = new LongOpenHashSet();
        for (int i = 0; i < chunks.length(); i++) {
            var chunk = chunks.get(i);
            if (chunk == null)
                continue;
            var pos = chunk.getPos();
            if (cache.getChunk(pos.x(), pos.z(), ChunkStatus.FULL, false) != chunk)
                continue;
            loaded.add(ChunkPos.pack(pos.x(), pos.z()));
            var sections = chunk.getSections();
            for (int y = 0; y < sections.length; y++)
                if (sections[y].hasOnlyAir())
                    empty.add(SectionPos.asLong(pos.x(), y + chunk.getMinSectionY(), pos.z()));
        }
        graph.updateLoadedChunks(loaded, none);
        graph.updateEmptySections(empty, none);
    }
}
