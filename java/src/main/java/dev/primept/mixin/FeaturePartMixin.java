package dev.primept.mixin;
import com.mojang.blaze3d.vertex.PoseStack;
import com.mojang.blaze3d.vertex.VertexConsumer;
import dev.primept.FeatureModels;
import dev.primept.WorldFeatures;
import java.util.List;
import net.minecraft.client.model.geom.ModelPart;
import org.spongepowered.asm.mixin.*;
import org.spongepowered.asm.mixin.injection.*;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(ModelPart.class)
abstract class FeaturePartMixin implements FeatureModels.Part {
    @Shadow @Final private List<ModelPart.Cube> cubes;
    @Unique private long primept$part;
    @Unique private FeatureModels.Geometry primept$geometry;
    @Inject(method = "<init>", at = @At("RETURN"))
    private void primept$bake(CallbackInfo ci) {
        primept$geometry = FeatureModels.geometry(cubes);
    }
    public FeatureModels.Geometry primept$geometry() {
        return primept$geometry;
    }
    @Inject(method = "compile", at = @At("HEAD"), cancellable = true)
    private void primept$instance(PoseStack.Pose pose, VertexConsumer sink, int light, int overlay,
            int color, CallbackInfo ci) {
        if (primept$geometry != null && WorldFeatures.active()) {
            if (primept$part == 0)
                primept$part = WorldFeatures.allocateId();
            if (WorldFeatures.model(primept$geometry, primept$part, pose.pose(), sink, color))
                ci.cancel();
        }
    }
}
