package dev.primept.mixin;

import dev.primept.WorldFeatures;
import net.minecraft.client.renderer.blockentity.state.BlockEntityRenderState;
import net.minecraft.client.renderer.entity.state.EntityRenderState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Unique;

/** 来源跟随提取的状态对象，供稍后提交同一姿态及其嵌套模型时读取。 */
@Mixin({EntityRenderState.class, BlockEntityRenderState.class})
abstract class FeatureStateMixin implements WorldFeatures.SourceState {
    @Unique private long primept$source;
    public long primept$sourceId() {
        return primept$source;
    }
    public void primept$sourceId(long id) {
        primept$source = id;
    }
}
