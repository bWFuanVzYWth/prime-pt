package dev.primept;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;

/** 薄事件所有者：按真实world身份/列/section合并，成功提交后才释放冻结的PDI1批。 */
final class SourceChanges {
    static final int BLOCK_RANGE=1,SECTION_SOURCE=2,COLUMN_SOURCE=3,BIOME_COLUMN=4,WORLD_TINT=5,SOURCE_OVERFLOW=6;
    private static final int MAX_NODES=1<<19;
    private final int maxNodes;
    private final IdentityHashMap<Object,World> worlds=new IdentityHashMap<>();
    private final Packet packet=new Packet();
    private int nodes;
    private Stage staged;
    SourceChanges(){this(MAX_NODES);}
    SourceChanges(int maxNodes){if(maxNodes<2)throw new IllegalArgumentException("事件节点额度至少为2");this.maxNodes=maxNodes;}
    private record Stage(Object world,ByteBuffer bytes){}
    private static final class World {
        final LinkedHashMap<Long,Column> columns=new LinkedHashMap<>();
        boolean tint,overflow;
        int nodes;
    }
    private static final class Column {
        final int x,z;
        final LinkedHashMap<Integer,Range> sections=new LinkedHashMap<>();
        boolean source,biome;
        int minY,maxY;
        Column(int x,int z){this.x=x;this.z=z;}
    }
    private static final class Range {
        int min,max;
        boolean whole;
        Range(int local,boolean whole){min=whole?0:local;max=whole?4095:local;this.whole=whole;}
        void include(int local){min=pack(Math.min(min&15,local&15),Math.min(min>>>8,local>>>8),Math.min((min>>>4)&15,(local>>>4)&15));max=pack(Math.max(max&15,local&15),Math.max(max>>>8,local>>>8),Math.max((max>>>4)&15,(local>>>4)&15));}
    }
    static int pack(int x,int y,int z){return x|z<<4|y<<8;}
    private World world(Object owner){return worlds.computeIfAbsent(owner,key->new World());}
    private static long key(int x,int z){return ((long)x<<32)|(z&0xffffffffL);}
    private Column column(World world,int x,int z){
        var key=key(x,z);var column=world.columns.get(key);
        if(column==null){column=new Column(x,z);world.columns.put(key,column);world.nodes++;nodes++;}
        return column;
    }
    private void overflow(World world){nodes-=world.nodes;world.nodes=0;world.columns.clear();world.tint=false;world.overflow=true;}
    private void bound(World world){if(nodes>maxNodes)overflow(world);}

    /** 精确block源变化；不在Java计算邻域或原版模型。同section仅保变化AABB。 */
    synchronized void block(Object owner,int x,int y,int z){
        var world=world(owner);if(world.overflow)return;
        int sx=x>>4,sy=y>>4,sz=z>>4;var column=column(world,sx,sz);
        if(column.source&&sy>=column.minY&&sy<column.maxY)return;
        int local=pack(x&15,y&15,z&15);var range=column.sections.get(sy);
        if(range==null){column.sections.put(sy,new Range(local,false));world.nodes++;nodes++;}
        else if(!range.whole)range.include(local);
        bound(world);
    }
    /** 只有完整section来源信息时使用；覆盖同key的block范围通知。 */
    synchronized void section(Object owner,int x,int y,int z){
        var world=world(owner);if(world.overflow)return;var column=column(world,x,z);
        if(column.source&&y>=column.minY&&y<column.maxY)return;
        var previous=column.sections.put(y,new Range(0,true));if(previous==null){world.nodes++;nodes++;}
        bound(world);
    }
    /** chunk加载/卸载的真实列范围；取消逐section及邻列枚举，biome单独标记。 */
    synchronized void column(Object owner,int x,int z,int minY,int maxY,boolean biome){
        if(minY>=maxY)throw new IllegalArgumentException("source列高度域为空");
        var world=world(owner);if(world.overflow)return;var column=column(world,x,z);
        if(column.source){column.minY=Math.min(column.minY,minY);column.maxY=Math.max(column.maxY,maxY);}else{column.source=true;column.minY=minY;column.maxY=maxY;}
        column.biome|=biome;
        var entries=column.sections.entrySet().iterator();while(entries.hasNext()){int y=entries.next().getKey();if(y>=column.minY&&y<column.maxY){entries.remove();world.nodes--;nodes--;}}
        bound(world);
    }
    /** 只失效实际替换的biome列，不将普通block变化等同biome变化。 */
    synchronized void biome(Object owner,int x,int z){var world=world(owner);if(world.overflow)return;column(world,x,z).biome=true;bound(world);}
    /** 原版颜色缓存/配置变化，不清压缩方块源或原始biome。 */
    synchronized void all(Object owner){var world=world(owner);if(!world.overflow)world.tint=true;}

    /** 返回冻结借用批；未accepted时重复返回同一内容，失败不会丢掉已移出的通知。 */
    synchronized ByteBuffer batch(Object owner,int limit){
        if(limit<1||limit>16384)throw new IllegalArgumentException("事件批容量无效");
        if(staged!=null){if(staged.world==owner)return staged.bytes.duplicate().order(ByteOrder.LITTLE_ENDIAN);staged=null;}
        var obsolete=worlds.entrySet().iterator();while(obsolete.hasNext()){var entry=obsolete.next();if(entry.getKey()!=owner){nodes-=entry.getValue().nodes;obsolete.remove();}}
        var world=worlds.get(owner);if(world==null)return null;
        packet.clear();packet.putInt(0x31494450);packet.putInt(1);packet.putInt(0);packet.putInt(0);
        int count=0;
        if(world.overflow){write(SOURCE_OVERFLOW,0,0,0,0,0);world.overflow=false;count++;}
        if(world.tint&&count<limit){write(WORLD_TINT,0,0,0,0,0);world.tint=false;count++;}
        var columns=world.columns.entrySet().iterator();
        while(count<limit&&columns.hasNext()){
            var column=columns.next().getValue();
            if(column.source){write(COLUMN_SOURCE,column.x,column.minY,column.z,column.maxY,1|(column.biome?2:0));column.source=false;column.biome=false;count++;}
            else if(column.biome){write(BIOME_COLUMN,column.x,0,column.z,0,0);column.biome=false;count++;}
            var sections=column.sections.entrySet().iterator();
            while(count<limit&&sections.hasNext()){
                var entry=sections.next();var range=entry.getValue();write(range.whole?SECTION_SOURCE:BLOCK_RANGE,column.x,entry.getKey(),column.z,range.whole?0:range.min,range.whole?0:range.max);sections.remove();world.nodes--;nodes--;count++;
            }
            if(!column.source&&!column.biome&&column.sections.isEmpty()){columns.remove();world.nodes--;nodes--;}
        }
        if(count==0)return null;
        packet.patchInt(8,count);staged=new Stage(owner,packet.finish().asReadOnlyBuffer().order(ByteOrder.LITTLE_ENDIAN));
        return staged.bytes.duplicate().order(ByteOrder.LITTLE_ENDIAN);
    }
    /** Native全批校验/应用成功后确认；在途新通知进入独立pending表，不随旧批丢弃。 */
    synchronized void accepted(){if(staged==null)throw new IllegalStateException("没有待确认事件批");staged=null;}
    /** 新资源/世界域完成发布，旧通知已不属于当前输入域。 */
    synchronized void clear(){worlds.clear();nodes=0;staged=null;}
    private void write(int kind,int x,int y,int z,int arg0,int arg1){packet.putInt(kind);packet.putInt(x);packet.putInt(y);packet.putInt(z);packet.putInt(arg0);packet.putInt(arg1);}
}
