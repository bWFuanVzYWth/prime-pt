package dev.primept.mixin;

import dev.primept.TerrainSnapshot;
import dev.primept.WorldFeatures;
import java.util.LinkedHashSet;
import java.util.Set;
import java.util.concurrent.atomic.AtomicReferenceArray;
import net.minecraft.client.Camera;
import net.minecraft.client.multiplayer.ClientChunkCache;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.LevelRenderer;
import net.minecraft.client.renderer.SectionOcclusionGraph;
import net.minecraft.client.renderer.state.level.LevelRenderState;
import net.minecraft.world.level.chunk.LevelChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Unique;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(ClientChunkCache.class)
abstract class ChunkCacheMixin implements TerrainSnapshot.Cache {
    @Unique private AtomicReferenceArray<LevelChunk> primept$chunks;

    @Unique private Set<LevelChunk> primept$occupied;
    public void primept$bindChunks(AtomicReferenceArray<LevelChunk> chunks) {
        primept$chunks = chunks;
        primept$occupied = null;
    }
    public void primept$releaseBlockEntities() {
        primept$occupied = null;
    }
    public void primept$blockEntitiesChanged(LevelChunk chunk) {
        if (primept$occupied == null)
            return;
        if (chunk.getBlockEntities().isEmpty())
            primept$occupied.remove(chunk);
        else
            primept$occupied.add(chunk);
    }
    @Inject(method = "replaceWithPacketData", at = @At("RETURN"))
    private void primept$loaded(CallbackInfoReturnable<LevelChunk> ci) {
        var chunk = ci.getReturnValue();
        if (chunk != null)
            primept$blockEntitiesChanged(chunk);
    }

    public void primept$blockEntities(ClientLevel level, LevelRenderer renderer, Camera camera,
            float partial, LevelRenderState output) {
        var dispatcher = renderer.blockEntityRenderDispatcher();
        var globals = level.getGloballyRenderedBlockEntities();
        var cache = (ClientChunkCache) (Object) this;
        if (primept$occupied == null) {
            primept$occupied = new LinkedHashSet<>();
            for (int i = 0; i < primept$chunks.length(); i++) {
                var chunk = primept$chunks.get(i);
                if (chunk != null)
                    primept$blockEntitiesChanged(chunk);
            }
        }
        for (var iter = primept$occupied.iterator(); iter.hasNext();) {
            var chunk = iter.next();
            var p = chunk.getPos();
            if (cache.getChunk(p.x(), p.z(), ChunkStatus.FULL, false) != chunk) {
                iter.remove();
                continue;
            }
            for (var be : chunk.getBlockEntities().values()) {
                if (be.isRemoved() || globals.contains(be))
                    continue;
                var state = dispatcher.tryExtractRenderState(be, partial, null, false);
                if (state != null) {
                    output.blockEntityRenderStates.add(state);
                    WorldFeatures.link(state, be);
                }
            }
        }
        for (var be : globals)
            if (!be.isRemoved()) {
                var state = dispatcher.tryExtractRenderState(be, partial, null, true);
                if (state != null) {
                    output.blockEntityRenderStates.add(state);
                    WorldFeatures.link(state, be);
                }
            }
    }

    public void primept$restoreTerrain(SectionOcclusionGraph graph) {
        TerrainSnapshot.restore((ClientChunkCache) (Object) this, primept$chunks, graph);
    }
}
