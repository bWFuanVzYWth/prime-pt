package dev.primept.mixin;
import dev.primept.FeatureModels;
import java.util.Map;
import net.minecraft.client.model.geom.EntityModelSet;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.builders.LayerDefinition;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;
@Mixin(EntityModelSet.class)
public interface FeatureModelSetMixin extends FeatureModels.Roots {
    @Accessor("roots") Map<ModelLayerLocation, LayerDefinition> primept$roots();
}
