package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import net.minecraft.client.model.geom.EntityModelSet;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.ModelPart;
import net.minecraft.client.model.geom.builders.LayerDefinition;

/** 每个资源世代从实际注册的模型层捕获各骨骼的静态四边形，共享相同的不可变模板。 */
public final class FeatureModels {
    public interface Part {
        Geometry primept$geometry();
    }
    public interface Roots {
        Map<ModelLayerLocation, LayerDefinition> primept$roots();
    }
    public static final class Geometry {
        final byte[] bytes;
        private final int hash;
        private long generation;
        private int index;
        Geometry(byte[] data) {
            bytes = data;
            hash = Arrays.hashCode(data);
        }
        public int id() {
            if (current == null)
                return -1;
            if (generation != current.version) {
                generation = current.version;
                index = current.models.getOrDefault(this, -1);
            }
            return index;
        }
        @Override
        public int hashCode() {
            return hash;
        }
        @Override
        public boolean equals(Object value) {
            return value instanceof Geometry g && Arrays.equals(bytes, g.bytes);
        }
    }
    private static FeatureModels current;
    private final long version = current == null ? 1 : Math.incrementExact(current.version);
    private final HashMap<Geometry, Integer> models = new HashMap<>();
    private final Packet packet = new Packet();
    public static Geometry geometry(List<ModelPart.Cube> cubes) {
        int count = 0;
        for (var cube : cubes) {
            for (var polygon : cube.polygons)
                if (polygon.vertices().length != 4)
                    return null;
            count = Math.addExact(count, cube.polygons.length);
        }
        if (count == 0)
            return null;
        var data = ByteBuffer.allocate(Math.multiplyExact(80, count)).order(ByteOrder.LITTLE_ENDIAN);
        for (var cube : cubes)
            for (var polygon : cube.polygons)
                for (var v : polygon.vertices())
                    data.putFloat(v.worldX())
                            .putFloat(v.worldY())
                            .putFloat(v.worldZ())
                            // 无纹理的原版模型层可以使用 0×0 的 UV 画布。
                            .putFloat(Float.isFinite(v.u()) ? v.u() : 0)
                            .putFloat(Float.isFinite(v.v()) ? v.v() : 0);
        return new Geometry(data.array());
    }
    static FeatureModels prepare(EntityModelSet set) {
        var result = new FeatureModels();
        result.packet.putInt(0x314d4650);
        result.packet.putInt(0);
        for (var layer : ((Roots) set).primept$roots().keySet()) {
            for (var part : set.bakeLayer(layer).getAllParts()) {
                var geometry = ((Part) (Object) part).primept$geometry();
                if (geometry == null
                        || result.models.putIfAbsent(geometry, result.models.size()) != null)
                    continue;
                result.packet.putInt(geometry.bytes.length / 80);
                result.packet.putBytes(geometry.bytes);
            }
        }
        result.packet.patchInt(4, result.models.size());
        return result;
    }
    ByteBuffer packet() {
        return packet.finish();
    }
    void publish() {
        current = this;
        packet.discard();
    }
}
