package dev.primept.mixin;
import com.mojang.renderpearl.backend.vulkan.VulkanGpuTexture;
import dev.primept.WorldFeatures;
import net.minecraft.client.particle.Particle;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.level.block.entity.BlockEntity;
import org.spongepowered.asm.mixin.*;
@Mixin({Entity.class, BlockEntity.class, Particle.class, VulkanGpuTexture.class})
abstract class FeatureOwnerMixin implements WorldFeatures.Owner {
    @Unique private long primept$identity;
    public long primept$id() {
        if (primept$identity == 0)
            primept$identity = WorldFeatures.allocateId();
        return primept$identity;
    }
}
