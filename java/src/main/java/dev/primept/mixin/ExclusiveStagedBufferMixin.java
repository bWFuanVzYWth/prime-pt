package dev.primept.mixin;

import com.mojang.blaze3d.vertex.VertexConsumer;
import dev.primept.WorldFeatures;
import java.util.List;
import net.minecraft.client.renderer.StagedVertexBuffer;
import org.spongepowered.asm.mixin.Final;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(StagedVertexBuffer.class)
public abstract class ExclusiveStagedBufferMixin implements WorldFeatures.Pages {
    @Shadow @Final private List<StagedVertexBuffer.Draw> draws;
    @Shadow protected abstract void finishLastVertexBuilder();

    @Inject(method = "getVertexBuilder", at = @At("RETURN"))
    private void primept$builder(
            StagedVertexBuffer.Draw draw, CallbackInfoReturnable<VertexConsumer> ci) {
        if (WorldFeatures.active())
            WorldFeatures.builder(ci.getReturnValue(), draw);
    }
    public void primept$finish() {
        finishLastVertexBuilder();
    }
    @Override
    public void primept$drain() {
        finishLastVertexBuilder();
        for (var draw : draws) ((ExclusiveStagedDrawAccess) draw).primept$freeVertexData();
    }
}
