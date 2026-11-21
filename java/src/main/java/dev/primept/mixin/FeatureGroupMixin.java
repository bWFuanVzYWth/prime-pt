package dev.primept.mixin;
import com.llamalad7.mixinextras.injector.wrapoperation.*;
import dev.primept.WorldFeatures;
import java.util.List;
import net.minecraft.client.renderer.feature.FeatureFrameContext;
import net.minecraft.client.renderer.feature.RenderTypeFeatureRenderer;
import net.minecraft.client.renderer.feature.submit.SubmitNode;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
@Mixin(RenderTypeFeatureRenderer.class)
abstract class FeatureGroupMixin {
    @WrapOperation(method = "prepareGroup",
            at = @At(value = "INVOKE",
                    target = "Lnet/minecraft/client/renderer/feature/"
                             + "RenderTypeFeatureRenderer;buildGroup(Lnet/minecraft/client/renderer/"
                             + "feature/FeatureFrameContext;Ljava/util/List;)V"))
    private void
    primept$source(RenderTypeFeatureRenderer<?> self, FeatureFrameContext context,
            List<? extends SubmitNode> nodes, Operation<Void> build) {
        if (!WorldFeatures.active()) {
            build.call(self, context, nodes);
            return;
        }
        var pages = (WorldFeatures.Pages) context.stagedVertexBuffer();
        for (var node : nodes) {
            WorldFeatures.building(node);
            try {
                if (node instanceof net.minecraft.client.renderer.feature.ItemFeatureRenderer.Submit item) {
                    WorldFeatures.item(item);
                } else {
                    build.call(self, context, List.of(node));
                    pages.primept$finish();
                }
            } finally {
                WorldFeatures.built();
            }
        }
    }
}
