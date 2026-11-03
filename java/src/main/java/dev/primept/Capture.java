package dev.primept;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.client.renderer.block.dispatch.BlockStateModelPart;
import net.minecraft.client.resources.model.geometry.BakedQuad;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.RenderShape;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.status.ChunkStatus;

/** 只捕获引擎请求的 LOD0 section；请求和窗口资源由 native 唯一管理。 */
final class Capture {
    private static final long BUDGET_NS = 3_000_000;
    private final ArrayList<BlockStateModelPart> parts = new ArrayList<>();
    private final RandomSource random = RandomSource.create();
    private final BlockPos.MutableBlockPos position = new BlockPos.MutableBlockPos();
    private final Packet packet = new Packet();
    private ClientLevel level;
    boolean select(ClientLevel current) {
        if (current == level) return false;
        level = current;
        return true;
    }

    void invalidate() { level = null; }

    ByteBuffer next(Minecraft minecraft, ByteBuffer requests, int count) {
        if (count < 1 || count > 64 || requests.remaining() < count * 24)
            throw new IllegalArgumentException("捕获请求数量无效");
        packet.clear();
        putInt(0x32545250); putInt(count);
        long started = System.nanoTime();
        int completed = 0;
        for (int i = 0; i < count; i++) {
            int offset = requests.position() + i * 24;
            long id = requests.getLong(offset);
            if (id == 0 || requests.getInt(offset + 20) != 0)
                throw new IllegalArgumentException("捕获请求 id 或 reserved 无效");
            packet.putLong(id);
            captureSection(minecraft, requests.getInt(offset + 8), requests.getInt(offset + 12), requests.getInt(offset + 16));
            completed++;
            // 单个宿主section求值不能中断；至少完成一个，未捕获ID下帧继续。
            if (System.nanoTime() - started >= BUDGET_NS) break;
        }
        packet.patchInt(4, completed);
        return packet.finish();
    }

    private void captureSection(Minecraft minecraft, int sx, int sy, int sz) {
        putInt(sx); putInt(sy); putInt(sz);
        int countAt = packet.position();
        putInt(0);
        if (sy < level.getMinSectionY() || sy > level.getMaxSectionY()) return;
        var chunk = level.getChunkSource().getChunk(sx, sz, ChunkStatus.FULL, false);
        if (chunk == null) {
            packet.patchInt(countAt, -1);
            return;
        }
        var section = chunk.getSection(chunk.getSectionIndexFromSectionY(sy));
        if (section.hasOnlyAir()) return;
        int blocks = 0;
        var models = minecraft.getModelManager().getBlockStateModelSet();
        for (int z = 0; z < 16; z++) for (int y = 0; y < 16; y++) for (int x = 0; x < 16; x++) {
            BlockState state = section.getBlockState(x, y, z);
            if (state.isAir() || state.getRenderShape() != RenderShape.MODEL) continue;
            position.set(sx * 16 + x, sy * 16 + y, sz * 16 + z);
            parts.clear();
            random.setSeed(state.getSeed(position));
            models.get(state).collectParts(random, parts);
            int blockAt = packet.position();
            putInt(x | y << 4 | z << 8);
            int quadsAt = packet.position();
            putInt(0);
            var offset = state.getOffset(position);
            packet.putFloat((float) offset.x); packet.putFloat((float) offset.y); packet.putFloat((float) offset.z);
            int quads = 0;
            for (var part : parts) {
                for (BakedQuad quad : part.getQuads(null)) { quad(minecraft, state, quad); quads++; }
                for (var face : Direction.values()) {
                    if (!Block.shouldRenderFace(state, level.getBlockState(position.relative(face)), face)) continue;
                    for (BakedQuad quad : part.getQuads(face)) { quad(minecraft, state, quad); quads++; }
                }
            }
            if (quads == 0) packet.position(blockAt);
            else { packet.patchInt(quadsAt, quads); blocks++; }
        }
        packet.patchInt(countAt, blocks);
    }

    private void quad(Minecraft minecraft, BlockState state, BakedQuad quad) {
        var source = quad.materialInfo().isTinted()
                ? minecraft.getBlockColors().getTintSource(state, quad.materialInfo().tintIndex()) : null;
        int tint = source == null ? -1 : source.colorInWorld(state, level, position);
        packet.quad(quad, tint);
    }

    private void putInt(int value) { packet.putInt(value); }
}
