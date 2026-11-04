package dev.primept.mixin;

import dev.primept.Renderer;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.LevelChunk;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ClientLevel.class)
abstract class LevelMixin {
    @Inject(method = "setBlocksDirty", at = @At("HEAD"),cancellable=true)
    private void primept$block(BlockPos pos, BlockState previous,BlockState current,CallbackInfo callback) {
        if(previous!=current)Renderer.dirty((ClientLevel) (Object) this,pos);
        // 此入口是Level.setBlock的真实源变化通知；截断下游27次Java网格依赖枚举。
        callback.cancel();
    }

    @Inject(method = "setSectionDirtyWithNeighbors", at = @At("HEAD"),cancellable=true)
    private void primept$section(int x, int y, int z, CallbackInfo callback) {
        Renderer.section((ClientLevel) (Object) this, x, y, z);
        callback.cancel();
    }

    @Inject(method = "onChunkLoaded", at = @At("RETURN"))
    private void primept$load(ChunkPos pos, CallbackInfo callback) { Renderer.column((ClientLevel) (Object) this, pos.x(), pos.z()); }

    @Inject(method = "unload", at = @At("RETURN"))
    private void primept$unload(LevelChunk chunk, CallbackInfo callback) { Renderer.column((ClientLevel) (Object) this, chunk.getPos().x(), chunk.getPos().z()); }

    @Inject(method = "clearTintCaches", at = @At("RETURN"))
    private void primept$tint(CallbackInfo callback) { Renderer.invalidate((ClientLevel) (Object) this); }
}
