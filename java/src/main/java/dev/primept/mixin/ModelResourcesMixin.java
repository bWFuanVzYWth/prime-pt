package dev.primept.mixin;

import dev.primept.Renderer;
import net.minecraft.client.resources.model.ModelManager;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/** apply返回后模型、流体和atlas均已发布，此处是唯一全局资源准备入口。 */
@Mixin(ModelManager.class)
abstract class ModelResourcesMixin {
    @Inject(method="apply",at=@At("RETURN"))
    private void primept$prepareResources(CallbackInfo callback){Renderer.prepareResources();}
}
