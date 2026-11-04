package dev.primept;

import java.lang.reflect.Field;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import java.util.Map;
import net.minecraft.client.color.block.BlockTintSource;
import net.minecraft.client.color.block.BlockTintSources;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.core.registries.Registries;
import net.minecraft.world.level.GrassColor;
import net.minecraft.world.level.FoliageColor;
import net.minecraft.world.level.DryFoliageColor;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.block.state.properties.DoubleBlockHalf;
import net.minecraft.world.level.levelgen.synth.SimplexNoise;

/** 26.3真实字段源；zoom、colormap、noise及box blend均由Rust计算。 */
final class SourceBiomes {
    private static final Class<?> CONSTANT = BlockTintSources.constant(0).getClass();
    private static final Class<?> CONSTANT_PAIR = BlockTintSources.constant(0,0).getClass();
    private static final Map<Class<?>,Field> CONSTANTS = Map.of(
            CONSTANT,field(CONSTANT,"arg$1"),CONSTANT_PAIR,field(CONSTANT_PAIR,"val$colorInWorld"));
    // 按实际factory类识别；自定义实现保留真实批量宿主回调，不猜block名称。
    private static final Map<Class<?>,Integer> KINDS = Map.ofEntries(
            Map.entry(BlockTintSources.grass().getClass(),1),Map.entry(BlockTintSources.grassBlock().getClass(),1),
            Map.entry(BlockTintSources.sugarCane().getClass(),1),Map.entry(BlockTintSources.foliage().getClass(),2),
            Map.entry(BlockTintSources.dryFoliage().getClass(),3),Map.entry(BlockTintSources.water().getClass(),4),
            Map.entry(BlockTintSources.doubleTallGrass().getClass(),5),Map.entry(BlockTintSources.redstone().getClass(),6),
            Map.entry(BlockTintSources.stem().getClass(),7),Map.entry(CONSTANT,8),Map.entry(CONSTANT_PAIR,8),
            Map.entry(BlockTintSources.waterParticles().getClass(),8));
    private static final Field SEED=field(BiomeManager.class,"biomeZoomSeed");
    private static final Field CLIMATE=field(Biome.class,"climateSettings");
    private static final Field TEMPERATURE=field(CLIMATE.getType(),"temperature"),DOWNFALL=field(CLIMATE.getType(),"downfall");
    private static final Field[] MAPS={field(GrassColor.class,"pixels"),field(FoliageColor.class,"pixels"),field(DryFoliageColor.class,"pixels")};
    private static final Class<?> NOISE=net.minecraft.world.level.levelgen.synth.GradientNoise.class;
    private static final Field PERMUTATION=field(NOISE,"perms"),OFFSET_X=field(NOISE,"offsetX"),OFFSET_Y=field(NOISE,"offsetY");
    private record Key(int state,int slot) {}
    private record Recipe(int kind,int value) {}
    private final long generation;
    private final LinkedHashMap<Key,Recipe> recipes=new LinkedHashMap<>();
    private final IdentityHashMap<Biome,Integer> palette=new IdentityHashMap<>();
    private final Packet resources=new Packet(),worldSource=new Packet(),response=new Packet();
    private ClientLevel world;
    private boolean accepted;

    SourceBiomes(long generation){if(generation==0)throw new IllegalArgumentException("tint资源世代为零");this.generation=generation;}
    /** 完整资源遍历时注册实际模型slot或流体-1；不执行最终颜色函数。 */
    void register(BlockState state,int slot,BlockTintSource source){
        if(accepted)throw new IllegalStateException("已发布tint资源不能追加recipe");
        if(slot< -1)throw new IllegalArgumentException("tint slot小于-1");
        int kind=source==null?8:KINDS.getOrDefault(source.getClass(),9);
        int value=switch(kind){
            case 5 -> state.getValue(BlockStateProperties.DOUBLE_BLOCK_HALF)==DoubleBlockHalf.UPPER?1:0;
            case 6 -> state.getValue(BlockStateProperties.POWER);
            case 7 -> state.getValue(BlockStateProperties.AGE_7);
            case 8 -> {var f=source==null?null:CONSTANTS.get(source.getClass());yield f==null?-1:(int)get(f,source);}
            default -> 0;
        };
        var key=new Key(Block.getId(state),slot);var recipe=new Recipe(kind,value);
        var previous=recipes.putIfAbsent(key,recipe);
        if(previous!=null&&!previous.equals(recipe))throw new IllegalStateException("同资源世代tint源身份不稳定");
    }
    /** PTR1由Catalog候选一并校验和发布；运行期不追加recipe。 */
    ByteBuffer packet(){
        if(accepted)throw new IllegalStateException("已发布tint资源不能重新捕获");
        resources.clear();resources.putInt(0x31525450);resources.putInt(263);resources.putLong(generation);
        writeDefinitions(resources);
        resources.putInt(recipes.size());
        for(var entry:recipes.entrySet()){
            var key=entry.getKey();var recipe=entry.getValue();
            resources.putInt(key.state);resources.putInt(key.slot);resources.putInt(recipe.kind);resources.putInt(recipe.value);
        }
        return resources.finish();
    }
    /** Rust已接收完整资源后释放临时recipe和字典包；世界应答只保留biome身份。 */
    void accepted(){accepted=true;recipes.clear();resources.discard();}
    /** 世界进入/资源重载时一次捕获完整registry palette与seed；不求最终tint。 */
    ByteBuffer world(ClientLevel level){
        var biomes=new ArrayList<Biome>();var next=new IdentityHashMap<Biome,Integer>();
        for(var biome:level.registryAccess().lookupOrThrow(Registries.BIOME))if(!next.containsKey(biome)){next.put(biome,biomes.size());biomes.add(biome);}
        worldSource.clear();worldSource.putInt(0x31574250);worldSource.putInt(263);worldSource.putLong(generation);
        worldSource.putLong((long)get(SEED,level.getBiomeManager()));worldSource.putInt(biomes.size());
        for(var biome:biomes)writeBiome(worldSource,biome);
        // 在完整字段捕获成功后替换Java应答字典；Native拒绝时调用者重新提交同包。
        palette.clear();palette.putAll(next);world=level;
        return worldSource.finish();
    }
    /** 按请求mask低位到高位仅复制真实quart biome身份，结果为紧凑u32数组。 */
    ByteBuffer respond(ClientLevel level,ByteBuffer requests,int count){
        if(level!=world)throw new IllegalStateException("biome世界源未准备或已切换");
        if(count<0||requests.remaining()<Math.multiplyExact(count,24))throw new IllegalArgumentException("biome请求长度无效");
        response.clear();var manager=level.getBiomeManager();int base=requests.position();
        for(int i=0;i<count;i++){
            int at=base+i*24;if(requests.getInt(at+12)!=0)throw new IllegalArgumentException("biome请求保留位非零");
            int x=Math.multiplyExact(requests.getInt(at),4),y=Math.multiplyExact(requests.getInt(at+4),4),z=Math.multiplyExact(requests.getInt(at+8),4);
            long mask=requests.getLong(at+16);
            while(mask!=0){int local=Long.numberOfTrailingZeros(mask);mask&=mask-1;
                var biome=manager.getNoiseBiomeAtQuart(x+(local&3),y+(local>>>4),z+((local>>>2)&3)).value();
                var id=palette.get(biome);if(id==null)throw new IllegalStateException("quart引用未准备的biome身份");response.putInt(id);
            }
        }
        return response.finish();
    }
    @SuppressWarnings("removal") // 26.3原版grass modifier使用此实际noise源。
    static void writeDefinitions(Packet out){
        var noise=Biome.BIOME_INFO_NOISE;if(noise.getClass()!=SimplexNoise.class)throw new IllegalStateException("未知biome noise源");
        var permutation=(byte[])get(PERMUTATION,noise);
        for(int i=0;i<256;i++)out.putInt(permutation[i]&255);
        out.putDouble((double)get(OFFSET_X,noise));out.putDouble((double)get(OFFSET_Y,noise));
        for(var field:MAPS){var pixels=(int[])get(field,null);int count=Math.min(65536,pixels.length);out.putInt(count);for(int i=0;i<count;i++)out.putInt(pixels[i]);}
    }
    static void writeBiome(Packet out,Biome biome){
        var climate=get(CLIMATE,biome);var effects=biome.getSpecialEffects();
        var grass=effects.grassColorOverride();var foliage=effects.foliageColorOverride();var dry=effects.dryFoliageColorOverride();
        out.putFloat((float)get(TEMPERATURE,climate));out.putFloat((float)get(DOWNFALL,climate));out.putInt(effects.waterColor());
        out.putInt(grass.orElse(0));out.putInt(foliage.orElse(0));out.putInt(dry.orElse(0));
        out.putInt((grass.isPresent()?1:0)|(foliage.isPresent()?2:0)|(dry.isPresent()?4:0));
        out.putInt(switch(effects.grassColorModifier()){case NONE->0;case DARK_FOREST->1;case SWAMP->2;});
    }
    static Field field(Class<?> type,String name){try{var field=type.getDeclaredField(name);field.setAccessible(true);return field;}catch(ReflectiveOperationException e){throw new ExceptionInInitializerError(e);}}
    static Object get(Field field,Object owner){try{return field.get(owner);}catch(IllegalAccessException e){throw new IllegalStateException("biome源字段不可访问",e);}}
}
