package dev.primept;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.BitSet;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;
import net.minecraft.client.Minecraft;
import net.minecraft.client.renderer.block.BlockStateModelSet;
import net.minecraft.client.renderer.block.FluidModel;
import net.minecraft.client.renderer.block.dispatch.BlockStateModel;
import net.minecraft.client.renderer.block.dispatch.SingleVariant;
import net.minecraft.client.renderer.block.dispatch.WeightedVariants;
import net.minecraft.client.renderer.block.dispatch.multipart.MultiPartModel;
import net.minecraft.client.resources.model.SimpleModelWrapper;
import net.minecraft.client.resources.model.geometry.BakedQuad;
import net.minecraft.client.resources.model.geometry.QuadCollection;
import net.minecraft.client.renderer.texture.TextureAtlasSprite;
import net.minecraft.client.model.geom.builders.UVPair;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.util.BitStorage;
import net.minecraft.util.random.WeightedList;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.RenderShape;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.GlobalPalette;
import net.minecraft.world.level.chunk.Palette;
import net.minecraft.world.level.chunk.PalettedContainer;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.material.FlowingFluid;
import net.minecraft.world.phys.shapes.DiscreteVoxelShape;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

/** MC26.3字段字典；位置随机、剔除、流体网格与材质翻译由Rust消费。 */
final class SourceResources {
    private static SourceResources prepared;
    private static final Field DATA = field(PalettedContainer.class, "data");
    private static final Field STORAGE = field(DATA.getType(), "storage"), PALETTE = field(DATA.getType(), "palette");
    private static final Field MODELS = field(BlockStateModelSet.class, "modelByState");
    private static final Field SINGLE = field(SingleVariant.class, "model"), WEIGHTED = field(WeightedVariants.class, "list");
    private static final Field SHARED = field(MultiPartModel.class, "shared"), SELECTED = field(MultiPartModel.class, "models"), BLOCK_STATE = field(MultiPartModel.class, "blockState");
    private static final Method SELECT = method(SHARED.getType(), "selectModels", BlockState.class);
    private static final Field FACES = field(BlockBehaviour.BlockStateBase.class, "occlusionShapesByFace");
    private static final Field SOLID = field(BlockBehaviour.BlockStateBase.class, "solidRender");
    private static final Field CACHE = field(BlockBehaviour.BlockStateBase.class,"cache"), STURDY = field(CACHE.getType(),"faceSturdy");
    private static final Field VOXELS = field(VoxelShape.class, "shape");
    private static final Field[] QUADS = { field(QuadCollection.class,"down"),field(QuadCollection.class,"up"),field(QuadCollection.class,"north"),field(QuadCollection.class,"south"),field(QuadCollection.class,"west"),field(QuadCollection.class,"east"),field(QuadCollection.class,"unculled") };
    private final Minecraft minecraft;
    final long generation;
    private final Map<BlockState, BlockStateModel> modelSet;
    private final BitSet statesKnown = new BitSet();
    private final ArrayList<BlockState> states = new ArrayList<>();
    private final IdentityHashMap<Object,Integer> modelIds = new IdentityHashMap<>();
    private final ArrayList<ModelRow> models = new ArrayList<>();
    private final IdentityHashMap<VoxelShape,int[]> faceIds = new IdentityHashMap<>();
    private final ArrayList<FaceRow> faces = new ArrayList<>();
    private final IdentityHashMap<FluidModel,Integer> fluidIds = new IdentityHashMap<>();
    private final ArrayList<FluidModel> fluids = new ArrayList<>();
    private final IdentityHashMap<TextureAtlasSprite,Integer> spriteIds = new IdentityHashMap<>();
    private final ArrayList<TextureAtlasSprite> sprites = new ArrayList<>();
    private final Packet packet = new Packet();
    final SourceBiomes biomes;
    private boolean accepted;
    private record ModelRow(Object source, int kind, int[] children, int[] weights) {}
    private record FaceRow(VoxelShape shape, Direction.Axis normal) {}

    @SuppressWarnings("unchecked")
    SourceResources(Minecraft minecraft, long generation) {
        this.minecraft = minecraft; this.generation = generation;
        biomes=new SourceBiomes(generation);
        modelSet = (Map<BlockState,BlockStateModel>)get(MODELS,minecraft.getModelManager().getBlockStateModelSet());
        for (var sprite : Atlas.sprites()) sprite(sprite);
        for (BlockState state : Block.BLOCK_STATE_REGISTRY) state(state);
        for (BlockState state : states) registerTints(state);
    }

    @SuppressWarnings("unchecked")
    private void registerTints(BlockState state){
        var pending=new ArrayDeque<Integer>();var seen=new BitSet();var slots=new java.util.HashSet<Integer>();
        int root=modelIds.getOrDefault(modelSet.get(state),0);if(root!=0)pending.add(root);
        while(!pending.isEmpty()){
            int id=pending.removeFirst();if(seen.get(id))continue;seen.set(id);var row=models.get(id-1);
            for(int child:row.children)pending.addLast(child);
            if(row.kind==1){var model=(SimpleModelWrapper)row.source;for(Field field:QUADS)for(BakedQuad q:(List<BakedQuad>)get(field,model.quads()))if(q.materialInfo().isTinted())slots.add(q.materialInfo().tintIndex());}
        }
        for(int slot:slots)biomes.register(state,slot,minecraft.getBlockColors().getTintSource(state,slot));
        var fluid=state.getFluidState();if(!fluid.isEmpty()){
            var source=minecraft.getModelManager().getFluidStateModelSet().get(fluid).tintSource();
            if(source!=null)biomes.register(state,-1,source);
        }
    }

    static SourceResources prepareReload(Minecraft minecraft) { return new SourceResources(minecraft,Atlas.generation); }
    static SourceResources prepared() { if(prepared==null)throw new IllegalStateException("源资源世代尚未完成首次/重载准备");return prepared; }

    /** 锁内直接复制压缩字段到借用包；资源字典已全量准备，不临时展开或追加模型。 */
    void section(Packet out, LevelChunkSection section) {
        palette(out,section.getStates());
    }
    /** 与离屏fixture共用的实际压缩字段writer，不创建状态或storage副本。 */
    @SuppressWarnings("unchecked")
    static void palette(Packet out, PalettedContainer<BlockState> container) {
        container.acquire();
        try {
            Object data = get(DATA,container);
            var palette = (Palette<BlockState>)get(PALETTE,data);
            var storage = (BitStorage)get(STORAGE,data);
            boolean global = palette instanceof GlobalPalette<?>;
            int size=global?0:palette.getSize();long[] words=storage.getRaw();
            out.putInt(1);out.putInt(storage.getBits());out.putInt(size);out.putInt(words.length);
            for(int i=0;i<size;i++)out.putInt(Block.getId(palette.valueFor(i)));
            for(long word:words)out.putLong(word);
        } finally { container.release(); }
    }

    private void state(BlockState value) {
        int id=Block.getId(value);
        if (statesKnown.get(id)) return;
        model(modelSet.get(value));
        VoxelShape[] shapes=(VoxelShape[])get(FACES,value);
        for (var direction:Direction.values()) face(shapes[direction.ordinal()],direction.getAxis());
        var fluid=value.getFluidState();
        if (!fluid.isEmpty()) fluid(minecraft.getModelManager().getFluidStateModelSet().get(fluid));
        statesKnown.set(id); states.add(value);
    }

    /** 显式栈保留源DAG；不递归，也不在每个方块上collectParts。 */
    @SuppressWarnings("unchecked")
    private int model(Object source) {
        if (source==null) return 0;
        Integer known=modelIds.get(source); if(known!=null) return known;
        int first=models.size();
        var pending=new ArrayDeque<Object>(); pending.add(source);
        while(!pending.isEmpty()) {
            Object value=pending.removeFirst(); if(modelIds.containsKey(value)) continue;
            int id=models.size()+1; modelIds.put(value,id); models.add(null);
            int kind=0; Object[] children=new Object[0]; int[] weights=new int[0];
            if(value.getClass()==SingleVariant.class) {kind=4;children=new Object[]{get(SINGLE,value)};}
            else if(value.getClass()==WeightedVariants.class) {
                kind=2; var entries=((WeightedList<BlockStateModel>)get(WEIGHTED,value)).unwrap();
                children=new Object[entries.size()];weights=new int[entries.size()];
                for(int i=0;i<children.length;i++){children[i]=entries.get(i).value();weights[i]=entries.get(i).weight();}
            } else if(value.getClass()==MultiPartModel.class) {
                kind=3;var selected=(List<BlockStateModel>)get(SELECTED,value);
                if(selected==null) try {selected=(List<BlockStateModel>)SELECT.invoke(get(SHARED,value),get(BLOCK_STATE,value));SELECTED.set(value,selected);}
                catch(ReflectiveOperationException e){throw new IllegalStateException("Multipart源选择失败",e);}
                children=selected.toArray();
            } else if(value instanceof SimpleModelWrapper simple) {
                kind=1; for(Field field:QUADS) for(BakedQuad q:(List<BakedQuad>)get(field,simple.quads())) sprite(q.materialInfo().sprite());
            } else throw new IllegalStateException("尚未转发自定义方块模型源："+value.getClass().getName());
            int[] ids=new int[children.length];
            // 先分配后排队；实际row在本循环结束后由source引用查ID，允许共享与前向引用。
            for(Object child:children) if(child!=null&&!modelIds.containsKey(child)) pending.addLast(child);
            models.set(id-1,new ModelRow(value,kind,ids,weights));
        }
        for(int mi=first;mi<models.size();mi++){ModelRow row=models.get(mi);if(row.children.length>0) {
            Object[] children;
            if(row.kind==4) children=new Object[]{get(SINGLE,row.source)};
            else if(row.kind==2) children=((WeightedList<BlockStateModel>)get(WEIGHTED,row.source)).unwrap().stream().map(e->e.value()).toArray();
            else children=((List<?>)get(SELECTED,row.source)).toArray();
            for(int i=0;i<children.length;i++) row.children[i]=modelIds.get(children[i]);
        }}
        return modelIds.get(source);
    }

    private int sprite(TextureAtlasSprite value) {
        Integer known=spriteIds.get(value);if(known!=null)return known;
        int id=sprites.size()+1;spriteIds.put(value,id);sprites.add(value);return id;
    }
    private int fluid(FluidModel value) {
        Integer known=fluidIds.get(value);if(known!=null)return known;
        int id=fluids.size()+1;fluidIds.put(value,id);fluids.add(value);
        sprite(value.stillMaterial().sprite());sprite(value.flowingMaterial().sprite());
        if(value.overlayMaterial()!=null)sprite(value.overlayMaterial().sprite());return id;
    }
    private int face(VoxelShape value,Direction.Axis normal) {
        if(value==Shapes.empty())return 0;if(value==Shapes.block())return 1;
        int[] ids=faceIds.computeIfAbsent(value,v->new int[3]);
        if(ids[normal.ordinal()]==0){ids[normal.ordinal()]=faces.size()+2;faces.add(new FaceRow(value,normal));}
        return ids[normal.ordinal()];
    }

    /** 未发布定义保留到native明确接受；失败不会提前标记已发送。 */
    ByteBuffer packet() {
        if(accepted)return null;
        int[] counts={states.size(),models.size(),faces.size(),fluids.size(),sprites.size()};
        packet.clear();packet.putInt(0x314d5250);packet.putInt(263);packet.putLong(generation);packet.putInt(1);
        for(int count:counts)packet.putInt(count);
        for(var state:states)writeState(state);
        for(int i=0;i<models.size();i++)writeModel(i+1,models.get(i));
        for(int i=0;i<faces.size();i++)writeFace(i+2,faces.get(i));
        for(int i=0;i<fluids.size();i++)writeFluid(i+1,fluids.get(i));
        int unsupported=0;
        for(int i=0;i<sprites.size();i++){writeSprite(i+1,sprites.get(i));if(hasExternalPbr(sprites.get(i)))unsupported++;}
        if(unsupported!=0)System.getLogger("prime_pt").log(System.Logger.Level.WARNING,"资源世代 "+generation+" 包含 "+unsupported+" 个外部_n/_s sprite；当前仅消费内嵌原版PBR，外部LabPBR尚未接入");
        var tint=biomes.packet();packet.putInt(tint.remaining());packet.putBuffer(tint);
        return packet.finish();
    }
    void accepted(){accepted=true;prepared=this;packet.discard();biomes.accepted();states.clear();modelIds.clear();models.clear();faceIds.clear();faces.clear();fluidIds.clear();fluids.clear();spriteIds.clear();sprites.clear();}
    void empty(Packet out,BlockState air){if(!statesKnown.get(Block.getId(air)))throw new IllegalStateException("空section引用未准备状态");out.putInt(1);out.putInt(0);out.putInt(1);out.putInt(0);out.putInt(Block.getId(air));}

    private void writeState(BlockState s) {
        int flags=(s.isAir()?1:0)|((boolean)get(SOLID,s)?4:0)|(s.getRenderShape()!=RenderShape.MODEL?16:0)|(s.isSolid()?32:0);
        if(s.getBlock() instanceof net.minecraft.world.level.block.HalfTransparentBlock||s.getBlock() instanceof net.minecraft.world.level.block.LeavesBlock)flags|=64;
        if(s.getBlock() instanceof net.minecraft.world.level.block.IceBlock)flags|=128;
        String name=BuiltInRegistries.BLOCK.getKey(s.getBlock()).toString();
        if(s.getBlock()==net.minecraft.world.level.block.Blocks.GLASS||s.getBlock() instanceof net.minecraft.world.level.block.StainedGlassBlock||s.getBlock() instanceof net.minecraft.world.level.block.TintedGlassBlock)flags|=256;
        if(s.getBlock()==net.minecraft.world.level.block.Blocks.GLASS_PANE||s.getBlock() instanceof net.minecraft.world.level.block.StainedGlassPaneBlock)flags|=512;
        if(s.getBlock() instanceof net.minecraft.world.level.block.LeavesBlock)flags|=1024;
        var p=SourcePlacement.prepare(s);if(p.kind()!=0)flags|=2;
        packet.putInt(Block.getId(s));packet.putInt(flags);packet.putInt(modelIds.getOrDefault(modelSet.get(s),0));packet.putString(name);
        VoxelShape[] shapes=(VoxelShape[])get(FACES,s);for(var d:Direction.values())packet.putInt(face(shapes[d.ordinal()],d.getAxis()));
        Object cache=get(CACHE,s);int support=0x80000000;
        if(cache!=null){support=0;boolean[] sturdy=(boolean[])get(STURDY,cache);for(int i=0;i<sturdy.length;i++)if(sturdy[i])support|=1<<i;}
        packet.putInt(support);
        var f=s.getFluidState();String fn=BuiltInRegistries.FLUID.getKey(f.getType()).toString();
        packet.putInt(f.isEmpty()?0:fn.equals("minecraft:water")||fn.equals("minecraft:flowing_water")?1:fn.equals("minecraft:lava")||fn.equals("minecraft:flowing_lava")?2:3);
        packet.putInt(f.isEmpty()?0:fluidIds.get(minecraft.getModelManager().getFluidStateModelSet().get(f)));
        packet.putInt(f.hasProperty(FlowingFluid.LEVEL)?f.getValue(FlowingFluid.LEVEL):0);
        packet.putInt(f.hasProperty(FlowingFluid.FALLING)&&f.getValue(FlowingFluid.FALLING)?1:0);packet.putInt(s.getLightEmission());
        packet.putInt(p.kind());packet.putFloat(p.horizontal());packet.putFloat(p.vertical());packet.putInt(p.seed());
    }
    @SuppressWarnings("unchecked")
    private void writeModel(int id,ModelRow row) {
        packet.putInt(id);packet.putInt(row.kind);
        if(row.kind==4){packet.putInt(row.children[0]);return;}
        if(row.kind==2||row.kind==3){packet.putInt(row.children.length);for(int i=0;i<row.children.length;i++){if(row.kind==2)packet.putInt(row.weights[i]);packet.putInt(row.children[i]);}return;}
        if(row.kind!=1)return;
        var simple=(SimpleModelWrapper)row.source;int count=0;for(Field f:QUADS)count+=((List<?>)get(f,simple.quads())).size();packet.putInt(count);
        for(int face=0;face<7;face++)for(BakedQuad q:(List<BakedQuad>)get(QUADS[face],simple.quads())){
            var m=q.materialInfo();packet.putInt(face);packet.putInt(m.isTinted()?m.tintIndex():-1);packet.putInt(m.layer().ordinal());packet.putInt(spriteIds.get(m.sprite()));packet.putInt(m.lightEmission());
            for(int i=0;i<4;i++){var p=q.position(i);packet.putFloat(p.x());packet.putFloat(p.y());packet.putFloat(p.z());}
            for(int i=0;i<4;i++){long uv=q.packedUV(i);packet.putFloat(UVPair.unpackU(uv));packet.putFloat(UVPair.unpackV(uv));}
        }
    }
    private void writeFace(int id,FaceRow row) {
        int u=row.normal==Direction.Axis.X?2:0,v=row.normal==Direction.Axis.Y?2:1;
        var us=row.shape.getCoords(Direction.Axis.values()[u]);var vs=row.shape.getCoords(Direction.Axis.values()[v]);
        packet.putInt(id);packet.putInt(us.size());for(double c:us)packet.putDouble(c);packet.putInt(vs.size());for(double c:vs)packet.putDouble(c);
        int nu=us.size()-1,nv=vs.size()-1;long[] words=new long[(nu*nv+63)/64];var grid=(DiscreteVoxelShape)get(VOXELS,row.shape);int[] xyz=new int[3];
        for(int a=0;a<nu;a++)for(int b=0;b<nv;b++){xyz[u]=a;xyz[v]=b;if(grid.isFull(xyz[0],xyz[1],xyz[2])){int bit=a*nv+b;words[bit/64]|=1L<<(bit%64);}}
        packet.putInt(words.length);for(long word:words)packet.putLong(word);
    }
    private void writeFluid(int id,FluidModel f) {
        packet.putInt(id);packet.putInt(f.layer().ordinal());packet.putInt(f.tintSource()!=null?1:0);
        packet.putInt(spriteIds.get(f.stillMaterial().sprite()));packet.putInt(spriteIds.get(f.flowingMaterial().sprite()));packet.putInt(f.overlayMaterial()==null?0:spriteIds.get(f.overlayMaterial().sprite()));
    }
    private void writeSprite(int id,TextureAtlasSprite s) {
        var c=s.contents();packet.putInt(id);packet.putString(c.name().toString());packet.putFloat(s.getU0());packet.putFloat(s.getV0());packet.putFloat(s.getU1());packet.putFloat(s.getV1());packet.putInt(c.width());packet.putInt(c.height());
        var resource=minecraft.getResourceManager().getResource(net.minecraft.resources.Identifier.fromNamespaceAndPath(c.name().getNamespace(),"textures/"+c.name().getPath()+".png"));
        var animation=SourceAnimation.metadata(c);
        packet.putInt((resource.isPresent()&&resource.get().sourcePackId().equals("vanilla")?1:0)|(animation.interpolate()?2:0));
        var frames=animation.frames();packet.putInt(frames.size());
        for(Object frame:frames){packet.putInt(SourceAnimation.index(frame));packet.putInt(SourceAnimation.duration(frame));}
        packet.putInt(1);
        var image=((dev.primept.mixin.SpriteAccess)c).primept$image();packet.putInt(image.getWidth());packet.putInt(image.getHeight());
        if(frames.isEmpty())packet.putInt(0);
        else{int count=Math.multiplyExact(Math.multiplyExact(image.getWidth(),image.getHeight()),4);packet.putInt(count);var pixels=image.getPixelBytes().duplicate();pixels.position(0).limit(count);packet.putBuffer(pixels);}
        packet.putInt(0);packet.putInt(0);
    }
    private boolean hasExternalPbr(TextureAtlasSprite sprite){
        var name=sprite.contents().name();var manager=minecraft.getResourceManager();
        return manager.getResource(net.minecraft.resources.Identifier.fromNamespaceAndPath(name.getNamespace(),"textures/"+name.getPath()+"_n.png")).isPresent()||manager.getResource(net.minecraft.resources.Identifier.fromNamespaceAndPath(name.getNamespace(),"textures/"+name.getPath()+"_s.png")).isPresent();
    }
    private static Field field(Class<?> owner,String name){try{var f=owner.getDeclaredField(name);f.setAccessible(true);return f;}catch(ReflectiveOperationException e){throw new ExceptionInInitializerError(e);}}
    private static Method method(Class<?> owner,String name,Class<?>...args){try{var m=owner.getDeclaredMethod(name,args);m.setAccessible(true);return m;}catch(ReflectiveOperationException e){throw new ExceptionInInitializerError(e);}}
    private static Object get(Field field,Object owner){try{return field.get(owner);}catch(IllegalAccessException e){throw new IllegalStateException("源字段不可访问",e);}}
}
