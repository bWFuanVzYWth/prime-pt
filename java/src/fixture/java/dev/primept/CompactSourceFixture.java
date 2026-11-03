package dev.primept;

import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.PalettedContainer;
import net.minecraft.world.level.chunk.Strategy;

/** 实际26.3 palette/storage经生产writer，附原版逐格读取结果；无客户端、窗口或GPU。 */
final class CompactSourceFixture {
    public static void main(String[] args)throws Exception{
        SharedConstants.tryDetectVersion();Bootstrap.bootStrap();
        verifyAnimationOrder();
        var states=new ArrayList<BlockState>();for(var state:Block.BLOCK_STATE_REGISTRY)states.add(state);
        reportSupport(states);
        for(var state:states){var placement=SourcePlacement.prepare(state);if(placement.kind()==3||placement.seed()==6)throw new AssertionError("原版位置规则未接："+state);}
        var out=new Packet();out.putInt(0x31464350);out.putInt(263);out.putInt(3);
        for(int pattern=0;pattern<3;pattern++){
            var palette=new PalettedContainer<>(Blocks.AIR.defaultBlockState(),Strategy.createForBlockStates(Block.BLOCK_STATE_REGISTRY));
            for(int i=0;i<4096;i++)if(pattern!=0){
                var state=pattern==1?(i%7==0?Blocks.WATER.defaultBlockState():i%3==0?Blocks.OAK_LEAVES.defaultBlockState():Blocks.STONE.defaultBlockState()):states.get(i%states.size());
                palette.set(i%16,i/256,i/16%16,state);
            }
            var packet=new Packet();packet.putInt(0x33545250);packet.putInt(1);packet.putLong(1);packet.putLong(pattern+1);packet.putInt(-2);packet.putInt(pattern-1);packet.putInt(4);
            SourceResources.palette(packet,palette);var bytes=packet.finish();out.putInt(bytes.remaining());out.putBuffer(bytes);
            for(int i=0;i<4096;i++)out.putInt(Block.getId(palette.get(i%16,i/256,i/16%16)));
        }
        var path=Path.of(args[0]);Files.createDirectories(path.toAbsolutePath().getParent());
        try(var file=FileChannel.open(path,StandardOpenOption.CREATE,StandardOpenOption.TRUNCATE_EXISTING,StandardOpenOption.WRITE)){
            var bytes=out.finish();while(bytes.hasRemaining())file.write(bytes);
        }
        System.out.println("PRIME_COMPACT_SOURCE_OK：3种实际palette、12,288格oracle，生产writer；无游戏/GPU");
    }
    private static void verifyAnimationOrder()throws Exception{
        var image=new com.mojang.blaze3d.platform.NativeImage(8,4,true);
        try(var contents=new net.minecraft.client.renderer.texture.SpriteContents(net.minecraft.resources.Identifier.withDefaultNamespace("prime_animated"),new net.minecraft.client.resources.metadata.animation.FrameSize(4,4),image)){
            var field=contents.getClass().getDeclaredField("animatedTexture");field.setAccessible(true);
            var type=Class.forName(contents.getClass().getName()+"$FrameInfo");var frame=type.getDeclaredConstructor(int.class,int.class);frame.setAccessible(true);
            var sequence=java.util.List.of(frame.newInstance(1,3),frame.newInstance(0,7));
            var constructor=field.getType().getDeclaredConstructors()[0];constructor.setAccessible(true);field.set(contents,constructor.newInstance(contents,sequence,2,true));
            var metadata=SourceAnimation.metadata(contents);
            if(metadata.firstFrame()!=1||!metadata.interpolate()||SourceAnimation.duration(metadata.frames().get(0))!=3||SourceAnimation.index(metadata.frames().get(1))!=0)throw new AssertionError("初始颜色必须使用真实timeline顺序[1,0]，不使用unique集合顺序");
        }
    }
    private static void reportSupport(ArrayList<BlockState> states)throws Exception{
        var cache=net.minecraft.world.level.block.state.BlockBehaviour.BlockStateBase.class.getDeclaredField("cache");cache.setAccessible(true);
        var dynamic=net.minecraft.world.level.block.state.BlockBehaviour.class.getDeclaredField("dynamicShape");dynamic.setAccessible(true);
        var unknown=new java.util.TreeMap<String,Integer>();int staticallyMissing=0;
        for(var state:states)if(cache.get(state)==null){String name=net.minecraft.core.registries.BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();unknown.merge(name,1,Integer::sum);if(!(boolean)dynamic.get(state.getBlock()))staticallyMissing++;}
        System.out.println("PRIME_SUPPORT_SOURCE states="+states.size()+" cacheNullStatic="+staticallyMissing+" unknown="+unknown);
    }
}
