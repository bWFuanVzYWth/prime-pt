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
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

@Mixin(ClientLevel.class)
abstract class LevelMixin {
    @Inject(method = "setBlock", at = @At("RETURN"))
    private void primept$block(BlockPos pos, BlockState state, int flags, int recursionLeft, CallbackInfoReturnable<Boolean> callback) {
        if (callback.getReturnValueZ()) Renderer.dirty(pos);
    }

    @Inject(method = "setSectionDirtyWithNeighbors", at = @At("HEAD"))
    private void primept$section(int x, int y, int z, CallbackInfo callback) {
        Renderer.section(x, y, z);
        Renderer.section(x - 1, y, z); Renderer.section(x + 1, y, z);
        Renderer.section(x, y - 1, z); Renderer.section(x, y + 1, z);
        Renderer.section(x, y, z - 1); Renderer.section(x, y, z + 1);
    }

    @Inject(method = "onChunkLoaded", at = @At("RETURN"))
    private void primept$load(ChunkPos pos, CallbackInfo callback) { Renderer.column(pos.x(), pos.z()); }

    @Inject(method = "unload", at = @At("RETURN"))
    private void primept$unload(LevelChunk chunk, CallbackInfo callback) { Renderer.column(chunk.getPos().x(), chunk.getPos().z()); }

    @Inject(method = "clearTintCaches", at = @At("RETURN"))
    private void primept$tint(CallbackInfo callback) { Renderer.reset(); }
}
