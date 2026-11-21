package dev.primept;

import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import net.fabricmc.api.EnvType;
import net.fabricmc.loader.impl.launch.knot.Knot;
import net.minecraft.SharedConstants;
import net.minecraft.client.model.geom.EntityModelSet;
import net.minecraft.client.renderer.LevelRenderer;
import net.minecraft.client.renderer.RenderPipelines;
import net.minecraft.server.Bootstrap;

/** 只初始化 Fabric 类变换，检查真实 26.3 模型与专用绘制的来源。全程不创建客户端或窗口。 */
public final class WorldFeatureFixture {
    public static void main(String[] args) throws Exception {
        var loader = new Knot(EnvType.CLIENT).init(new String[0]);
        loader.loadClass("dev.primept.WorldFeatureFixture$Assets")
                .getMethod("check", String.class)
                .invoke(null, args[0]);
    }
    public static final class Assets {
        public static void check(String file) throws Exception {
            SharedConstants.tryDetectVersion();
            Bootstrap.bootStrap();
            var loader = Assets.class.getClassLoader();
            for (String name : new String[] {"net.minecraft.client.renderer.extract.LevelExtractor",
                         "net.minecraft.client.renderer.LevelRenderer",
                         "net.minecraft.client.renderer.GameRenderer",
                         "net.minecraft.client.renderer.entity.EntityRenderDispatcher",
                         "net.minecraft.client.renderer.entity.state.EntityRenderState",
                         "net.minecraft.client.renderer.blockentity.state.BlockEntityRenderState",
                         "net.minecraft.client.renderer.blockentity.BlockEntityRenderDispatcher",
                         "net.minecraft.client.renderer.feature.FeatureRenderDispatcher",
                         "net.minecraft.client.renderer.feature.RenderTypeFeatureRenderer$Group",
                         "net.minecraft.client.renderer.feature.phase.SimpleFeatureRenderPhase",
                         "net.minecraft.client.renderer.feature.phase.TranslucentFeatureRenderPhase",
                         "net.minecraft.client.renderer.StagedVertexBuffer$Draw",
                         "net.minecraft.client.renderer.state.level.QuadParticleRenderState",
                         "net.minecraft.client.particle.SingleQuadParticle",
                         "net.minecraft.client.particle.QuadParticleGroup",
                         "net.minecraft.client.particle.ItemPickupParticleGroup$ParticleInstance",
                         "net.minecraft.client.particle.ElderGuardianParticleGroup$"
                         + "ElderGuardianParticleRenderState",
                         "net.minecraft.client.particle.ItemPickupParticleGroup$State",
                         "net.minecraft.client.particle.ElderGuardianParticleGroup$State",
                         "net.minecraft.client.multiplayer.ClientChunkCache",
                         "net.minecraft.world.level.chunk.LevelChunk",
                         "com.mojang.renderpearl.backend.vulkan.VulkanCommandEncoder",
                         "com.mojang.renderpearl.backend.vulkan.VulkanGpuTexture",
                         "net.minecraft.client.renderer.blockentity.StandingSignRenderer",
                         "net.minecraft.client.renderer.blockentity.BeaconRenderer",
                         "net.minecraft.client.renderer.item.ItemModelResolver",
                         "net.minecraft.client.renderer.item.ItemStackRenderState",
                         "net.minecraft.client.renderer.item.ItemStackRenderState$LayerRenderState",
                         "net.minecraft.client.renderer.entity.ItemEntityRenderer",
                         "net.minecraft.client.renderer.entity.layers.ItemInHandLayer",
                         "net.minecraft.client.renderer.entity.ItemFrameRenderer",
                         "net.minecraft.client.renderer.entity.PaintingRenderer",
                         "net.minecraft.client.renderer.MapRenderer"})
                Class.forName(name, false, loader);
            if (!WorldFeatures.SourceRenderer.class.isAssignableFrom(LevelRenderer.class))
                throw new AssertionError("未应用动态提交钩子");
            var lamp = net.minecraft.world.level.block.Blocks.SEA_LANTERN;
            var state = WorldFeatures.blockState(lamp.asItem(), null);
            if (net.minecraft.world.level.block.Block.stateById(state).getLightEmission() != 15
                    || WorldFeatures.blockState(net.minecraft.world.item.Items.STICK, null) != -1)
                throw new AssertionError("海晶灯物品必须保留亮度 15，非方块物品不应继承方块");
            var item = new net.minecraft.client.renderer.item.ItemStackRenderState();
            ((WorldFeatures.ItemLayers)item).primept$block(state);
            var itemLayer = (WorldFeatures.ItemLayers)item.newLayer();
            if (itemLayer.primept$block(-1) != state) throw new AssertionError("烘焙物品图层丢失方块身份");
            var set = EntityModelSet.vanilla();
            var catalog = FeatureModels.prepare(set);
            var bytes = catalog.packet();
            if (bytes.getInt(0) != 0x314d4650 || bytes.getInt(4) < 100)
                throw new AssertionError("部件目录为空");
            var path = Path.of(file);
            Files.createDirectories(path.getParent());
            int size = bytes.remaining(), count = bytes.getInt(4);
            try (var out = FileChannel.open(path, StandardOpenOption.CREATE, StandardOpenOption.WRITE,
                         StandardOpenOption.TRUNCATE_EXISTING)) {
                while (bytes.hasRemaining()) out.write(bytes);
            }
            var payload = bytes.duplicate().order(java.nio.ByteOrder.LITTLE_ENDIAN);
            payload.position(8);
            var catalogSize = payload.getInt(4);
            for (int i = 0; i < catalogSize; i++) {
                int floats = Math.multiplyExact(20, payload.getInt());
                for (int j = 0; j < floats; j++)
                    if (!Float.isFinite(payload.getFloat()))
                        throw new AssertionError("准备的模型含未定义的位置或 UV");
            }
            if (payload.hasRemaining())
                throw new AssertionError("模型目录尾部有多余字节");
            catalog.publish();
            int occurrences = 0;
            for (var layer : ((FeatureModels.Roots) set).primept$roots().keySet())
                for (var part : set.bakeLayer(layer).getAllParts()) {
                    var g = ((FeatureModels.Part) (Object) part).primept$geometry();
                    if (g == null)
                        continue;
                    if (g.id() < 0)
                        throw new AssertionError("真实部件与资源目录不匹配：" + layer);
                    occurrences++;
                }
            var text = RenderPipelines.TEXT_GRAYSCALE;
            var beam = RenderPipelines.BEACON_BEAM_TRANSLUCENT;
            int glyph = WorldFeatures.flags(text.getLocation().getPath(), false, true),
                column = WorldFeatures.flags(beam.getLocation().getPath(), true, false);
            if ((glyph & (8 | 16)) != (8 | 16) || (column & (8 | 32)) != (8 | 32))
                throw new AssertionError("文字或信标分类错误");
            System.out.println("模型出现 " + occurrences + " 次，资源准备去重后 " + count
                    + " 个 BLAS 输入，" + size + " 字节");
        }
    }
}
