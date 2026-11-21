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
import net.minecraft.client.Camera;
import net.minecraft.client.Minecraft;
import net.minecraft.client.particle.SingleQuadParticle;
import net.minecraft.client.renderer.StagedVertexBuffer;
import net.minecraft.client.renderer.feature.TextFeatureRenderer;
import net.minecraft.client.renderer.feature.submit.SubmitNode;
import net.minecraft.client.renderer.rendertype.PreparedRenderType;
import net.minecraft.client.renderer.rendertype.RenderType;
import net.minecraft.client.renderer.state.level.CameraRenderState;
import net.minecraft.client.renderer.texture.TextureAtlas;
import net.minecraft.util.LightCoordsUtil;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.Vec3;
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
    public interface SourceRenderer {
        void primept$features(CameraRenderState camera);
    }
    public interface Pages {
        void primept$finish();
        void primept$drain();
    }
    private record Material(int texture, int flags, int topology) {}
    private record Node(long owner, int part) {}
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
    private static long particleOwner, cameraHiddenOwner;
    private static int particleFace;
    private static long nextId, owner;
    private static int part, chunk, count;
    private static final Long2IntOpenHashMap boneOccurrences = new Long2IntOpenHashMap();
    private static boolean active, luminousText;
    public static synchronized long allocateId() {
        return nextId = Math.incrementExact(nextId);
    }
    public static boolean active() {
        return active;
    }
    public static void releaseWorld() {
        end();
        textureSlots.clear();
        textures.clear();
        billboards.clear();
        boneOccurrences.clear();
        blockAtlas = null;
        spans = new Packet(4096);
        packet = new Packet(4096);
    }
    public static void extraction() {
        cameraHiddenOwner = 0;
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
    /** 压墙的第三人称相机也可能进入观察实体；只隐藏该实体的直视命中。 */
    public static void cameraBody(Entity observer, Vec3 eye, float partial, boolean attached) {
        if (attached || intersectsCamera(eye, observer.getBoundingBox(), observer.position(),
                                observer.getPosition(partial))) {
            cameraHiddenOwner = ((Owner) observer).primept$id();
        }
    }
    static boolean intersectsCamera(Vec3 eye, AABB bounds, Vec3 current, Vec3 rendered) {
        return bounds.inflate(Camera.PROJECTION_Z_NEAR).contains(eye.x + (current.x - rendered.x),
                eye.y + (current.y - rendered.y), eye.z + (current.z - rendered.z));
    }
    private static int appearance(int flags) {
        return flags | (luminousText && (flags & 8) != 0 ? 4 : 0)
                | (owner != 0 && owner == cameraHiddenOwner ? 64 : 0);
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
    public static void tag(SubmitNode node) {
        if (active && owner != 0)
            nodes.putIfAbsent(node, new Node(owner, part++));
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
