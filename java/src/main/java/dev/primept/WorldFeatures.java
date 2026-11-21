package dev.primept;

import com.mojang.blaze3d.vertex.MeshData;
import com.mojang.blaze3d.vertex.VertexConsumer;
import com.mojang.renderpearl.api.textures.GpuTexture;
import com.mojang.renderpearl.api.textures.GpuTextureView;
import com.mojang.renderpearl.backend.vulkan.VulkanGpuTextureView;
import it.unimi.dsi.fastutil.longs.Long2IntOpenHashMap;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import net.minecraft.client.Minecraft;
import net.minecraft.client.particle.SingleQuadParticle;
import net.minecraft.client.renderer.StagedVertexBuffer;
import net.minecraft.client.renderer.feature.TextFeatureRenderer;
import net.minecraft.client.renderer.feature.ItemFeatureRenderer;
import net.minecraft.client.resources.model.geometry.ItemQuads;
import net.minecraft.client.renderer.SubmitNodeCollector;
import net.minecraft.client.renderer.state.level.PlayerRenderState;
import net.minecraft.core.component.DataComponents;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.block.Block;
import net.minecraft.client.renderer.feature.submit.SubmitNode;
import net.minecraft.client.renderer.rendertype.PreparedRenderType;
import net.minecraft.client.renderer.rendertype.RenderType;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import net.minecraft.client.renderer.texture.TextureAtlas;
import net.minecraft.util.LightCoordsUtil;
import org.joml.Matrix4f;

/** 从原版实际提交捕获来源身份、图元布局及 Sampler0；设备句柄只借给本次宿主帧。 */
public final class WorldFeatures {
    private WorldFeatures() {}
    public interface SourceState {
        long primept$sourceId();
        void primept$sourceId(long id);
    }
    public interface Owner {
        long primept$id();
    }
    public interface TextureVersion {
        long primept$revision();
        void primept$written();
    }
    public interface ItemLayers {
        int primept$block(int state);
        boolean primept$emits();
    }
    public interface HandRenderer {
        void primept$hands(CameraRenderState camera, PlayerRenderState player, SubmitNodeCollector collector);
    }
    public interface SourceRenderer {
        void primept$features(CameraRenderState camera, boolean viewHands);
    }
    public interface Pages {
        void primept$finish();
        void primept$drain();
    }
    private record Material(int texture, int flags, int topology) {}
    private record Node(long owner, int part, int block) {}
    private static Packet spans = new Packet(4096), packet = new Packet(4096);
    private static int submissionDepth;
    private static final IdentityHashMap<SubmitNode, Node> nodes = new IdentityHashMap<>();
    private static final IdentityHashMap<StagedVertexBuffer.Draw, Material> materials =
            new IdentityHashMap<>();
    private static final IdentityHashMap<GpuTextureView, Integer> textureSlots = new IdentityHashMap<>();
    private static final ArrayList<VulkanGpuTextureView> textures = new ArrayList<>();
    private static final IdentityHashMap<VertexConsumer, StagedVertexBuffer.Draw> builders =
            new IdentityHashMap<>();
    private static GpuTexture blockAtlas;
    public static void builder(VertexConsumer builder, StagedVertexBuffer.Draw draw) {
        builders.put(builder, draw);
    }
    public static boolean model(
            FeatureModels.Geometry geometry, long bone, Matrix4f pose, VertexConsumer sink, int color) {
        var material = materials.get(builders.get(sink));
        if (material == null || material.topology != 4)
            return false;
        int id = geometry.id();
        if (id < 0)
            return false;
        int piece = boneOccurrences.addTo(bone, 1);
        if (piece >= 4096 || part > 0xfffff)
            throw new IllegalStateException("模型部件数量溢出");
        spans.putLong(owner);
        spans.putInt(part << 12 | piece);
        spans.putInt(appearance(material.flags));
        spans.putInt(material.texture);
        spans.putInt(0);
        spans.putInt(1);
        spans.putInt(64);
        spans.putInt(0);
        spans.putInt(-1);
        spans.putInt(-1);
        spans.putLong(bone);
        spans.putInt(id);
        spans.putInt(color);
        for (int row = 0; row < 3; row++)
            for (int col = 0; col < 4; col++) spans.putFloat(pose.get(col, row));
        count++;
        return true;
    }
    private static final class Billboards {
        final Packet data = new Packet(4096);
        int count;
    }
    private static final LinkedHashMap<SingleQuadParticle.Layer, Billboards> billboards =
            new LinkedHashMap<>();
    private static long particleOwner;
    private static int particleFace;
    private static long nextId, owner;
    private static int part, chunk, count;
    private static final int[] handParts = new int[2];
    private static final Long2IntOpenHashMap boneOccurrences = new Long2IntOpenHashMap();
    private static boolean active, luminousText;
    private static final long HAND_OWNER = allocateId();
    private static int itemBlock = -1;
    private static final IdentityHashMap<ItemLayers, Boolean> capturedLayers = new IdentityHashMap<>();
    public static int blockState(ItemStack stack) {
        return blockState(stack.getItem(), stack.get(DataComponents.BLOCK_STATE));
    }
    static int blockState(net.minecraft.world.item.Item item, net.minecraft.world.item.component.BlockItemStateProperties properties) {
        if (!(item instanceof BlockItem block)) return -1;
        var state = block.getBlock().defaultBlockState();
        return Block.getId(properties == null ? state : properties.apply(state));
    }
    public static boolean emits(int block, ItemQuads quads) {
        if (quads == null || quads.isEmpty()) return false;
        if (block >= 0 && Block.stateById(block).getLightEmission() != 0) return true;
        var resources = SourceResources.prepared();
        for (var q : quads.all()) {
            var m = q.materialInfo();
            if (m.lightEmission() != 0 || resources.emits(m.sprite())) return true;
        }
        return false;
    }
    public static int itemBlock(int state) {
        int old = itemBlock;
        itemBlock = state;
        return old;
    }
    public static void beginHands() {
        owner = HAND_OWNER;
        handParts[0] = handParts[1] = 0;
    }
    public static void submittedHands() {
        owner = 0;
    }
    public static boolean collectingHand() { return active && owner == HAND_OWNER; }
    public static boolean capturedHands() { return !capturedLayers.isEmpty(); }
    public static boolean capturedLayer(ItemLayers layer) { return capturedLayers.containsKey(layer); }
    public static int spanCount() { return count; }
    public static void captureLayer(ItemLayers layer, int before) {
        if (count != before) capturedLayers.put(layer, true);
    }
    public static synchronized long allocateId() {
        return nextId = Math.incrementExact(nextId);
    }
    public static boolean active() {
        return active;
    }
    public static void releaseWorld() {
        end();
        capturedLayers.clear();
        textureSlots.clear();
        textures.clear();
        billboards.clear();
        boneOccurrences.clear();
        blockAtlas = null;
        spans = new Packet(4096);
        packet = new Packet(4096);
    }
    public static void extraction() {
        particleOwner = 0;
        for (var group : billboards.values()) {
            group.count = 0;
            group.data.clear();
        }
    }
    public static void particleOwner(Object particle) {
        particleOwner = particle == null ? 0 : ((Owner) particle).primept$id();
        particleFace = 0;
    }
    public static boolean extractingParticle() {
        return particleOwner != 0;
    }
    public static void billboard(SingleQuadParticle.Layer layer, float x, float y, float z, float qx,
            float qy, float qz, float qw, float scale, float u0, float u1, float v0, float v1, int color,
            int light) {
        var group = billboards.computeIfAbsent(layer, k -> new Billboards());
        var out = group.data;
        out.putLong(particleOwner);
        out.putFloat(x);
        out.putFloat(y);
        out.putFloat(z);
        out.putFloat(qx);
        out.putFloat(qy);
        out.putFloat(qz);
        out.putFloat(qw);
        out.putFloat(scale);
        out.putFloat(u0);
        out.putFloat(u1);
        out.putFloat(v0);
        out.putFloat(v1);
        out.putInt(color);
        out.putInt((particleFace++ << 8) | (light & 255));
        group.count++;
    }
    public static void link(Object state, Object source) {
        if (state != null)
            ((SourceState) state).primept$sourceId(((Owner) source).primept$id());
    }
    private static int appearance(int flags) {
        return flags | (luminousText && (flags & 8) != 0 ? 4 : 0);
    }
    public static void submitting(Object state) {
        if (active && submissionDepth++ == 0) {
            owner = ((SourceState) state).primept$sourceId();
            part = 0;
        }
    }
    public static void particleModel(Object state) {
        submissionDepth = 1;
        owner = ((SourceState) state).primept$sourceId();
        part = 0;
    }
    public static void endParticleModels() {
        submissionDepth = 0;
        owner = 0;
    }
    public static void submitted() {
        if (active && --submissionDepth == 0)
            owner = 0;
    }
    public static void built() {
        owner = 0;
    }
    /** 手持物使用原版姿态直接写入世界一次；手臂、地图底板等继续由原版画在屏幕上。 */
    public static boolean collect(SubmitNode node) {
        if (collectingHand()) {
            if (node instanceof ItemFeatureRenderer.Submit item) {
                int side = item.displayContext().leftHand() ? 0 : 1;
                part = handParts[side]++ * 2 + side;
                item(item, itemBlock);
            }
            return true;
        }
        if (active && owner != 0)
            nodes.putIfAbsent(node, new Node(owner, part++, itemBlock));
        return false;
    }
    public static void building(SubmitNode node) {
        luminousText = node instanceof TextFeatureRenderer.Submit text
                && text.lightCoords() == LightCoordsUtil.FULL_BRIGHT;
        boneOccurrences.clear();
        Node n = nodes.get(node);
        owner = n == null ? 0 : n.owner;
        part = n == null ? 0 : n.part;
        chunk = 0;
    }
    public static void begin() {
        active = true;
        submissionDepth = 0;
        owner = 0;
        part = 0;
        chunk = 0;
        count = 0;
        capturedLayers.clear();
        itemBlock = -1;
        nodes.clear();
        materials.clear();
        builders.clear();
        textureSlots.clear();
        textures.clear();
        spans.clear();
        blockAtlas = Minecraft.getInstance()
                             .getTextureManager()
                             .getTexture(TextureAtlas.LOCATION_BLOCKS)
                             .getTexture();
    }
    public static void end() {
        active = false;
        owner = 0;
        nodes.clear();
        materials.clear();
        builders.clear();
    }
    private static int texture(GpuTextureView view) {
        if (view == null)
            return -1;
        if (view.texture() == blockAtlas)
            return -2;
        Integer old = textureSlots.get(view);
        if (old != null)
            return old;
        if (!(view instanceof VulkanGpuTextureView source))
            throw new IllegalStateException("原版纹理视图未绑定 Vulkan");
        int index = textures.size();
        textures.add(source);
        textureSlots.put(view, index);
        return index;
    }
    static int flags(String pipeline, boolean blends, boolean cutout) {
        int flags = blends ? 2 : cutout ? 1 : 0;
        if (pipeline.startsWith("pipeline/text"))
            return 1 | 8 | (pipeline.contains("grayscale") ? 16 : 0);
        if (pipeline.startsWith("pipeline/beacon_beam"))
            return 2 | 4 | 8 | 32;
        if (pipeline.contains("emissive") || pipeline.contains("eyes") || pipeline.contains("flame")
                || pipeline.contains("lightning"))
            flags |= 4;
        return flags;
    }
    public static void register(
            StagedVertexBuffer.Draw draw, RenderType type, PreparedRenderType prepared) {
        if (!active || materials.containsKey(draw) || type.isOutline())
            return;
        String name = type.pipeline().getLocation().getPath();
        if (name.contains("glint") || name.contains("shadow") || name.contains("outline")
                || name.contains("water_mask") || name.contains("crumbling"))
            return;
        int topology = switch (type.primitiveTopology().name()) {
            case "QUADS" -> 4;
            case "TRIANGLES" -> 3;
            case "TRIANGLE_STRIP" -> 5;
            case "TRIANGLE_FAN" -> 6;
            case "LINES" -> 7;
            default -> 0;
        };
        if (topology == 0)
            return;
        GpuTextureView view = null;
        for (var b : prepared.textures())
            if (b.name().equals("Sampler0")) {
                view = b.textureView();
                break;
            }
        materials.put(draw,
                new Material(texture(view),
                        flags(name, type.hasBlending(),
                                type.pipeline().getShaderDefines().values().containsKey("ALPHA_CUTOUT")),
                        topology));
    }
    /** 直接取烘焙面的真实 sprite、方块状态和自光亮度，所有面共享一次姿态。 */
    public static void item(ItemFeatureRenderer.Submit node) {
        var source = nodes.get(node);
        item(node, source == null ? -1 : source.block);
    }
    private static void item(ItemFeatureRenderer.Submit node, int block) {
        if (node.outlineColor() != 0 || node.quads().isEmpty()) return;
        var resources = SourceResources.prepared();
        var textures = Minecraft.getInstance().getTextureManager();
        spans.putLong(owner);
        spans.putInt(part);
        spans.putInt(0);
        spans.putInt(-1);
        spans.putInt(2);
        spans.putInt(node.quads().size());
        spans.putInt(96);
        spans.putInt(0);
        spans.putInt(-1);
        spans.putInt(-1);
        spans.putInt(block);
        var pose = node.pose().pose();
        for (int row = 0; row < 3; row++)
            for (int col = 0; col < 4; col++) spans.putFloat(pose.get(col, row));
        var tints = node.tintLayers();
        for (var quad : node.quads()) {
            var m = quad.materialInfo();
            var sprite = m.sprite();
            int texture = sprite.atlasLocation().equals(TextureAtlas.LOCATION_BLOCKS) ? -2
                    : texture(textures.getTexture(sprite.atlasLocation()).getTextureView());
            int i = m.tintIndex();
            int tint = m.isTinted() && i >= 0 && i < tints.length ? tints[i] : -1;
            spans.putInt(texture);
            spans.putInt(resources.itemSprite(sprite));
            spans.putInt(m.layer().ordinal() | (m.lightEmission() << 8));
            spans.quad(quad, tint);
        }
        count++;
    }
    public static void mesh(StagedVertexBuffer.Draw draw, MeshData data) {
        if (!active)
            return;
        var m = materials.get(draw);
        if (m == null)
            return;
        var state = data.drawState();
        var format = state.format();
        var pos = format.getElement("Position");
        if (pos == null || state.vertexCount() == 0)
            return;
        var color = format.getElement("Color");
        var uv = format.getElement("UV0");
        int piece = chunk++;
        if (part > 0xfffff || piece >= 4096)
            throw new IllegalStateException("同一来源的子网格数量溢出");
        spans.putLong(owner);
        spans.putInt((part << 12) | piece);
        spans.putInt(appearance(m.flags));
        spans.putInt(m.texture);
        spans.putInt(m.topology);
        spans.putInt(state.vertexCount());
        spans.putInt(format.getVertexSize());
        spans.putInt(pos.offset());
        spans.putInt(color == null ? -1 : color.offset());
        spans.putInt(uv == null ? -1 : uv.offset());
        spans.putBuffer(data.vertexBuffer());
        count++;
    }
    static ByteBuffer packet(CameraRenderState camera) {
        for (var entry : billboards.entrySet()) {
            var layer = entry.getKey();
            var group = entry.getValue();
            if (group.count == 0)
                continue;
            int slot = texture(Minecraft.getInstance()
                            .getTextureManager()
                            .getTexture(layer.textureAtlasLocation())
                            .getTextureView());
            spans.putLong(0);
            spans.putInt(0);
            spans.putInt(layer.translucent() ? 2 : 1);
            spans.putInt(slot);
            spans.putInt(1);
            spans.putInt(group.count);
            spans.putInt(64);
            spans.putInt(0);
            spans.putInt(-1);
            spans.putInt(-1);
            spans.putBuffer(group.data.finish());
            count++;
        }
        packet.clear();
        packet.putInt(0x31465450);
        packet.putInt(textures.size());
        packet.putInt(count);
        packet.putInt(0);
        packet.putDouble(camera.pos.x);
        packet.putDouble(camera.pos.y);
        packet.putDouble(camera.pos.z);
        for (var t : textures) {
            var tex = t.texture();
            packet.putLong(t.vkImageView());
            packet.putLong(((Owner) tex).primept$id());
            packet.putLong(((TextureVersion) tex).primept$revision());
            packet.putInt(t.getWidth(0));
            packet.putInt(t.getHeight(0));
            packet.putInt(tex.getFormat().name().contains("SRGB") ? 1 : 0);
            packet.putInt(0);
        }
        packet.putBuffer(spans.finish());
        return packet.finish();
    }
}
