package dev.primept;

import it.unimi.dsi.fastutil.longs.LongOpenHashSet;
import java.lang.reflect.Field;
import java.util.concurrent.atomic.AtomicReferenceArray;
import net.minecraft.SharedConstants;
import net.minecraft.client.multiplayer.ClientChunkCache;
import net.minecraft.client.renderer.SectionOcclusionGraph;
import net.minecraft.core.SectionPos;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.chunk.LevelChunkSection;

/** 使用真实缓存查询与可见性图；无窗口、无GPU，不构造Minecraft客户端。 */
final class TerrainSnapshotFixture {
    public static void main(String[] arguments) throws Exception {
        SharedConstants.tryDetectVersion();
        net.minecraft.server.Bootstrap.bootStrap();
        var cache = blank(ClientChunkCache.class);
        var type = Class.forName("net.minecraft.client.multiplayer.ClientChunkCache$Storage");
        var constructor = type.getDeclaredConstructor(ClientChunkCache.class, int.class);
        constructor.setAccessible(true);
        var storage = constructor.newInstance(cache, 2);
        field(ClientChunkCache.class, "storage").set(cache, storage);
        @SuppressWarnings("unchecked")
        var chunks = (AtomicReferenceArray<LevelChunk>) field(type, "chunks").get(storage);
        var index = type.getDeclaredMethod("getIndex", int.class, int.class);
        index.setAccessible(true);
        for (var chunk : new LevelChunk[] {chunk(0, 0, -4, true, false), chunk(-1, 1, -4, false, true), chunk(4, 0, -4, true)})
            chunks.set((int) index.invoke(storage, chunk.getPos().x(), chunk.getPos().z()), chunk);
        cache.flipUpdateTrackingSets();
        cache.flipUpdateTrackingSets();
        var graph = new SectionOcclusionGraph();
        graph.waitAndReset(null);
        TerrainSnapshot.restore(cache, chunks, graph);
        require(field(SectionOcclusionGraph.class, "loadedChunks").get(graph)
                .equals(LongOpenHashSet.of(ChunkPos.pack(0, 0), ChunkPos.pack(-1, 1))), "恢复当前可查询列，排除窗口外旧槽");
        require(field(SectionOcclusionGraph.class, "emptySections").get(graph)
                .equals(LongOpenHashSet.of(SectionPos.asLong(0, -4, 0), SectionPos.asLong(-1, -3, 1))), "使用真实最低高度恢复空section");
        require(cache.addedLoadedChunks().isEmpty() && cache.addedEmptySections().isEmpty(), "恢复不得重复投递共享加载事件");
        System.out.println("原版恢复夹具通过：日志耗尽、缓存现有列、空section和窗口外旧槽");
    }

    private static LevelChunk chunk(int x, int z, int min, boolean... empty) throws Exception {
        var chunk = blank(LevelChunk.class);
        var sections = new LevelChunkSection[empty.length];
        for (int i = 0; i < sections.length; i++) {
            sections[i] = blank(LevelChunkSection.class);
            field(LevelChunkSection.class, "nonEmptyBlockCount").setShort(sections[i], (short) (empty[i] ? 0 : 1));
        }
        field(ChunkAccess.class, "chunkPos").set(chunk, new ChunkPos(x, z));
        field(ChunkAccess.class, "sections").set(chunk, sections);
        field(ChunkAccess.class, "levelHeightAccessor").set(chunk, LevelHeightAccessor.create(min * 16, empty.length * 16));
        return chunk;
    }

    private static Field field(Class<?> owner, String name) throws Exception {
        var field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field;
    }

    private static <T> T blank(Class<T> type) throws Exception {
        // 仅夹具隔离数据字段；生产路径始终使用宿主构造的对象。
        var unsafe = Class.forName("sun.misc.Unsafe");
        return type.cast(unsafe.getMethod("allocateInstance", Class.class).invoke(field(unsafe, "theUnsafe").get(null), type));
    }

    private static void require(boolean value, String message) {
        if (!value) throw new AssertionError(message);
    }
}
