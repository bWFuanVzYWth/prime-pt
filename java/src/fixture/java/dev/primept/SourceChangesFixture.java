package dev.primept;

import java.nio.ByteBuffer;
import net.minecraft.core.SectionPos;

/** 生产PDI1合并/冻结/确认行为及真实26.3负坐标oracle；无客户端或GPU。 */
final class SourceChangesFixture {
    public static void main(String[] arguments){
        coordinates();coalescing();columns();overlappingEdit();editPriority();retry();worlds();largeView();overflow();
        System.out.println("PDI1生产事件：真实坐标、同key合并、编辑优先、失败重试、在途新事件、万section与一次溢出补偿通过");
    }
    private static int count(ByteBuffer bytes){
        if(bytes==null||bytes.getInt(0)!=0x31494450||bytes.getInt(4)!=1||bytes.getInt(12)!=0||bytes.remaining()!=16+24*bytes.getInt(8))throw new AssertionError("PDI1完整布局错误");
        return bytes.getInt(8);
    }
    private static void coordinates(){
        var changes=new SourceChanges();Object world=new Object();int n=0;
        for(int x:new int[]{-30000000,-17,-16,-1,0,15,16,17,30000000})for(int y:new int[]{-65,-16,-1,0,15,319}){
            int z=-x;changes.block(world,x,y,z);var bytes=changes.batch(world,16);
            int local=SectionPos.sectionRelative(x)|(SectionPos.sectionRelative(z)<<4)|(SectionPos.sectionRelative(y)<<8);
            if(count(bytes)!=1||bytes.getInt(16)!=SourceChanges.BLOCK_RANGE||bytes.getInt(20)!=SectionPos.blockToSectionCoord(x)||bytes.getInt(24)!=SectionPos.blockToSectionCoord(y)||bytes.getInt(28)!=SectionPos.blockToSectionCoord(z)||bytes.getInt(32)!=local||bytes.getInt(36)!=local)throw new AssertionError("原版section/local坐标不一致");
            changes.accepted();n++;
        }
        if(n!=54)throw new AssertionError();
    }
    private static void coalescing(){
        var changes=new SourceChanges();Object world=new Object();
        for(int i=0;i<50000;i++)changes.block(world,-1,-1,-1);
        changes.block(world,-16,-16,-16);var bytes=changes.batch(world,1024);
        if(count(bytes)!=1||bytes.getInt(16)!=1||bytes.getInt(32)!=0||bytes.getInt(36)!=4095)throw new AssertionError("同section应仅保inclusive AABB");
        changes.accepted();if(changes.batch(world,1024)!=null)throw new AssertionError("成功后重复发旧批");
    }
    private static void columns(){
        var changes=new SourceChanges();Object world=new Object();
        changes.section(world,0,1,0);changes.biome(world,0,0);changes.column(world,0,0,-4,20,true);
        var bytes=changes.batch(world,1024);
        if(count(bytes)!=1||bytes.getInt(16)!=3||bytes.getInt(24)!=-4||bytes.getInt(32)!=20||bytes.getInt(36)!=3)throw new AssertionError("列通知应支配该域source并保独立biome标志");
        changes.accepted();
    }
    private static void overlappingEdit(){
        for(boolean editFirst:new boolean[]{true,false}){
            var changes=new SourceChanges();Object world=new Object();
            if(editFirst)changes.block(world,1,1,1);
            changes.column(world,0,0,-4,20,true);
            if(!editFirst)changes.block(world,1,1,1);
            var bytes=changes.batch(world,1024);
            if(count(bytes)!=2||bytes.getInt(16)!=SourceChanges.BLOCK_RANGE||bytes.getInt(32)!=0x111
                    ||bytes.getInt(36)!=0x111||bytes.getInt(40)!=SourceChanges.COLUMN_SOURCE)
                throw new AssertionError("同一列里的真实编辑必须保留精确范围和独立身份");
            changes.accepted();
        }
    }
    private static void editPriority(){
        var changes=new SourceChanges();Object world=new Object();
        for(int i=0;i<4096;i++)changes.column(world,i,0,-4,20,true);
        changes.block(world,-16,0,0);
        var bytes=changes.batch(world,1);
        if(count(bytes)!=1||bytes.getInt(16)!=SourceChanges.BLOCK_RANGE||bytes.getInt(20)!=-1)
            throw new AssertionError("普通积压不能阻挡真实编辑先发");
        changes.accepted();
        changes.block(world,-32,0,0);
        bytes=changes.batch(world,1);
        if(count(bytes)!=1||bytes.getInt(16)!=SourceChanges.COLUMN_SOURCE||bytes.getInt(20)!=0)
            throw new AssertionError("单项额度下普通通知也须前进");
        changes.accepted();
        changes.block(world,-48,0,0);changes.block(world,-64,0,0);
        bytes=changes.batch(world,3);
        if(count(bytes)!=3||bytes.getInt(16)!=SourceChanges.BLOCK_RANGE||bytes.getInt(20)!=-2
                ||bytes.getInt(40)!=SourceChanges.BLOCK_RANGE||bytes.getInt(44)!=-3
                ||bytes.getInt(64)!=SourceChanges.COLUMN_SOURCE||bytes.getInt(68)!=1)
            throw new AssertionError("持续编辑时普通通知仍保留批内份额");
        changes.accepted();
        bytes=changes.batch(world,3);
        if(count(bytes)!=3||bytes.getInt(16)!=SourceChanges.BLOCK_RANGE||bytes.getInt(20)!=-4
                ||bytes.getInt(40)!=SourceChanges.COLUMN_SOURCE||bytes.getInt(44)!=2
                ||bytes.getInt(64)!=SourceChanges.COLUMN_SOURCE||bytes.getInt(68)!=3)
            throw new AssertionError("优先队列用不完的额度必须给普通队列");
        changes.accepted();
    }
    private static void retry(){
        var changes=new SourceChanges();Object world=new Object();changes.block(world,1,1,1);
        var first=changes.batch(world,1);changes.block(world,2,2,2);
        if(!first.equals(changes.batch(world,1)))throw new AssertionError("未确认批必须原样重试");
        changes.accepted();var next=changes.batch(world,1);
        if(count(next)!=1||next.getInt(32)!=0x222||next.getInt(36)!=0x222)throw new AssertionError("确认旧批不能丢在途新事件");
        changes.accepted();
    }
    private static void worlds(){
        var changes=new SourceChanges();Object previous=new String("world"),current=new String("world");
        changes.section(previous,1,2,3);changes.section(current,4,5,6);var bytes=changes.batch(current,16);
        if(count(bytes)!=1||bytes.getInt(20)!=4)throw new AssertionError("world必须按identity过滤");changes.accepted();
    }
    private static void largeView(){
        var changes=new SourceChanges();Object world=new Object();for(int i=0;i<10000;i++)changes.section(world,i,0,0);
        int total=0;ByteBuffer bytes;while((bytes=changes.batch(world,1024))!=null){int count=count(bytes);for(int i=0;i<count;i++)if(bytes.getInt(16+i*24)!=2)throw new AssertionError("万section不应退化全源失效");total+=count;changes.accepted();}
        if(total!=10000)throw new AssertionError("分批丢失唯一section");
    }
    private static void overflow(){
        var changes=new SourceChanges(16);Object world=new Object();for(int i=0;i<1000;i++)changes.section(world,i,0,0);
        var bytes=changes.batch(world,1024);if(count(bytes)!=1||bytes.getInt(16)!=6)throw new AssertionError("受控容量应只产生一次安全补偿");
        changes.section(world,2048,0,0);changes.accepted();bytes=changes.batch(world,1024);
        if(count(bytes)!=1||bytes.getInt(16)!=2||bytes.getInt(20)!=2048)throw new AssertionError("补偿在途之后的新source必须独立保留");changes.accepted();
    }
}
