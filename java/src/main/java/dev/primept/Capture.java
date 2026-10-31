package dev.primept;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.BitSet;
import java.util.HashMap;
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

/** 每帧最多一个 4³ section 源包；保留源四边形，不生成顶点或三角形。 */
final class Capture {
    private record Cell(int x, int y, int z) {
        int distance(Cell other) {
            return Math.abs(x - other.x) + Math.abs(y - other.y) + Math.abs(z - other.z);
        }
    }
    private final HashMap<Cell, BitSet> dirty = new HashMap<>();
    private final ArrayList<BlockStateModelPart> parts = new ArrayList<>();
    private final RandomSource random = RandomSource.create();
    private final BlockPos.MutableBlockPos position = new BlockPos.MutableBlockPos();
    private final Packet packet = new Packet();
    private ClientLevel level;
    private Cell center;

    boolean select(ClientLevel current, BlockPos camera) {
        var next = new Cell(Math.floorDiv(camera.getX(), 64), Math.floorDiv(camera.getY(), 64), Math.floorDiv(camera.getZ(), 64));
        if (current == level && next.equals(center)) return false;
        level = current;
        center = next;
        dirty.clear();
        for (int z = -1; z <= 1; z++) for (int y = -1; y <= 1; y++) for (int x = -1; x <= 1; x++) {
            var sections = new BitSet(64);
            sections.set(0, 64);
            dirty.put(new Cell(next.x + x, next.y + y, next.z + z), sections);
        }
        return true;
    }

    void invalidate() { level = null; }

    void section(int sx, int sy, int sz) {
        if (center == null) return;
        var cell = new Cell(Math.floorDiv(sx, 4), Math.floorDiv(sy, 4), Math.floorDiv(sz, 4));
        if (Math.abs(cell.x - center.x) > 1 || Math.abs(cell.y - center.y) > 1 || Math.abs(cell.z - center.z) > 1) return;
        dirty.computeIfAbsent(cell, unused -> new BitSet(64))
                .set(Math.floorMod(sx, 4) | Math.floorMod(sy, 4) << 2 | Math.floorMod(sz, 4) << 4);
    }

    void block(BlockPos pos) {
        section(pos.getX() >> 4, pos.getY() >> 4, pos.getZ() >> 4);
        for (var direction : Direction.values()) {
            var neighbor = pos.relative(direction);
            section(neighbor.getX() >> 4, neighbor.getY() >> 4, neighbor.getZ() >> 4);
        }
    }

    void column(int x, int z) {
        if (center == null) return;
        for (int y = (center.y - 1) * 4; y < (center.y + 2) * 4; y++) {
            section(x, y, z);
            section(x - 1, y, z); section(x + 1, y, z);
            section(x, y, z - 1); section(x, y, z + 1);
        }
    }

    ByteBuffer next(Minecraft minecraft) {
        if (dirty.isEmpty()) return null;
        Cell selected = null;
        for (var cell : dirty.keySet())
            if (selected == null || cell.distance(center) < selected.distance(center)) selected = cell;
        var sections = dirty.remove(selected);
        packet.clear();
        putInt(0x31545250); putInt(sections.cardinality());
        for (int index = sections.nextSetBit(0); index >= 0; index = sections.nextSetBit(index + 1))
            captureSection(minecraft, selected.x * 4 + (index & 3), selected.y * 4 + (index >> 2 & 3), selected.z * 4 + (index >> 4));
        return packet.finish();
    }

    private void captureSection(Minecraft minecraft, int sx, int sy, int sz) {
        putInt(sx); putInt(sy); putInt(sz);
        int countAt = packet.position();
        putInt(0);
        if (sy < level.getMinSectionY() || sy > level.getMaxSectionY()) return;
        var chunk = level.getChunkSource().getChunk(sx, sz, ChunkStatus.FULL, false);
        if (chunk == null) return;
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
