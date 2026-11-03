package dev.primept;

import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.List;
import java.util.Random;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;
import net.minecraft.client.color.block.BlockColors;
import net.minecraft.client.color.block.BlockTintSource;
import net.minecraft.client.color.block.BlockTintSources;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.world.level.GrassColor;
import net.minecraft.world.level.FoliageColor;
import net.minecraft.world.level.DryFoliageColor;
import net.minecraft.world.level.biome.*;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;

/** 实际26.3数学oracle和生产字段writer，不构造Minecraft客户端或GPU。 */
final class TintFixture {
    public static void main(String[] args)throws Exception{
        SharedConstants.tryDetectVersion();Bootstrap.bootStrap();
        var out=new Packet();out.putInt(0x314d4254);out.putInt(263);
        int[] map=new int[65536];for(int i=0;i<map.length;i++)map[i]=0x80304000+i;
        GrassColor.init(map);FoliageColor.init(map);DryFoliageColor.init(map);
        SourceBiomes.writeDefinitions(out);
        var biomes=new ArrayList<Biome>();
        for(var modifier:BiomeSpecialEffects.GrassColorModifier.values())for(boolean overrides:new boolean[]{false,true})
            for(float[] climate:new float[][]{{-.4f,.8f},{.2f,.3f},{.8f,.9f},{1f,1f},{2f,1.1f}}){
                var e=new BiomeSpecialEffects.Builder().waterColor(0x407abcde).grassColorModifier(modifier);
                if(overrides)e.grassColorOverride(0x80983412).foliageColorOverride(0x30123456).dryFoliageColorOverride(0x40abcdef);
                biomes.add(new Biome.BiomeBuilder().temperature(climate[0]).downfall(climate[1]).specialEffects(e.build())
                        .mobSpawnSettings(MobSpawnSettings.EMPTY).generationSettings(BiomeGenerationSettings.EMPTY).build());
            }
        var chosen=new BlockPos.MutableBlockPos();var position=new BlockPos.MutableBlockPos();var holder=Holder.direct(biomes.getFirst());var random=new Random(0x6ab739de);
        out.putInt(4*4096);
        for(long seed:new long[]{0,-1,Long.MIN_VALUE,0x123456789abcdefL}){
            var manager=new BiomeManager((x,y,z)->{chosen.set(x,y,z);return holder;},seed);
            for(int i=0;i<4096;i++){
                if(i<2048)position.set((i&31)-16,(i>>>10)-1,((i>>>5)&31)-16);
                else position.set(random.nextInt(60000001)-30000000,random.nextInt(4096)-2048,random.nextInt(60000001)-30000000);
                manager.getBiome(position);out.putLong(seed);for(int v:new int[]{position.getX(),position.getY(),position.getZ(),chosen.getX(),chosen.getY(),chosen.getZ()})out.putInt(v);
            }
        }
        out.putInt(biomes.size());
        for(var biome:biomes){SourceBiomes.writeBiome(out,biome);out.putInt(512);
            for(int i=0;i<512;i++){int x=i<256?(i&15)-8:random.nextInt(60000001)-30000000,z=i<256?(i>>>4)-8:random.nextInt(60000001)-30000000;
                for(int v:new int[]{x,z,biome.getGrassColor(x,z),biome.getFoliageColor(),biome.getDryFoliageColor(),biome.getWaterColor()})out.putInt(v);
            }
        }
        var resources=new SourceBiomes(99);var states=new ArrayList<BlockState>();var slots=new ArrayList<Integer>();var expected=new ArrayList<Integer>();var colors=BlockColors.createDefault();
        for(int power=0;power<16;power++){var state=Blocks.REDSTONE_WIRE.defaultBlockState().setValue(BlockStateProperties.POWER,power);add(resources,states,slots,expected,state,0,colors.getTintSource(state,0));}
        for(int age=0;age<8;age++){var state=Blocks.MELON_STEM.defaultBlockState().setValue(BlockStateProperties.AGE_7,age);add(resources,states,slots,expected,state,0,colors.getTintSource(state,0));}
        var diamond=Blocks.DIAMOND_BLOCK.defaultBlockState();int[] calls={0};BlockTintSource custom=state->{calls[0]++;return 0x50123456;};
        var sources=List.of(BlockTintSources.constant(0x20769412),BlockTintSources.constant(0x8070ffff,0x40abcdef),BlockTintSources.waterParticles(),custom);
        for(int i=0;i<sources.size();i++)add(resources,states,slots,expected,diamond,i,sources.get(i));
        if(calls[0]!=1)throw new AssertionError("自定义回调只能由oracle显式执行一次");
        add(resources,states,slots,expected,Blocks.STONE.defaultBlockState(),99,null);
        ByteBuffer ptr=resources.packet();out.putInt(ptr.remaining());byte[] bytes=new byte[ptr.remaining()];ptr.get(bytes);out.putBytes(bytes);
        out.putInt(states.size());for(int i=0;i<states.size();i++){out.putInt(Block.getId(states.get(i)));out.putInt(slots.get(i));out.putInt(i==27?1:0);out.putInt(expected.get(i));}
        Path path=Path.of(args[0]);Files.createDirectories(path.toAbsolutePath().getParent());
        try(var file=FileChannel.open(path,StandardOpenOption.CREATE,StandardOpenOption.TRUNCATE_EXISTING,StandardOpenOption.WRITE)){var data=out.finish();while(data.hasRemaining())file.write(data);}
        System.out.println("26.3 tint oracle: "+path.toAbsolutePath()+" (16384 zoom,15360 biome samples,29 source recipes)");
    }
    private static void add(SourceBiomes resources,ArrayList<BlockState> states,ArrayList<Integer> slots,ArrayList<Integer> expected,BlockState state,int slot,BlockTintSource source){
        resources.register(state,slot,source);states.add(state);slots.add(slot);expected.add(source==null?-1:source.colorInWorld(state,null,BlockPos.ZERO));
    }
}
