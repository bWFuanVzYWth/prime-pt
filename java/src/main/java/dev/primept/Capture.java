package dev.primept;

import java.nio.ByteBuffer;
import net.minecraft.client.Minecraft;
import net.minecraft.client.multiplayer.ClientLevel;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.chunk.status.ChunkStatus;

/** 仅转发请求section的原始palette/storage；不逐block选择模型、剔面或展开quad。 */
final class Capture {
    private static final long BUDGET_NS=3_000_000;
    private final Packet packet=new Packet();
    private ClientLevel level;
    private SourceResources resources;
    private final Packet colors=new Packet();
    private final net.minecraft.core.BlockPos.MutableBlockPos tintPosition=new net.minecraft.core.BlockPos.MutableBlockPos();

    boolean select(ClientLevel current){if(current==level)return false;level=current;return true;}
    void invalidate(){level=null;resources=null;}

    ByteBuffer next(Minecraft minecraft,ByteBuffer requests,int count){
        if(count<1||count>64||requests.remaining()<count*24)throw new IllegalArgumentException("捕获请求数量无效");
        resources=SourceResources.prepared();
        packet.clear();packet.putInt(0x33545250);packet.putInt(count);packet.putLong(resources.generation);
        long started=System.nanoTime();int completed=0;
        for(int i=0;i<count;i++){
            int at=requests.position()+i*24;long id=requests.getLong(at);
            if(id==0||requests.getInt(at+20)!=0)throw new IllegalArgumentException("捕获票据无效");
            int sx=requests.getInt(at+8),sy=requests.getInt(at+12),sz=requests.getInt(at+16);
            packet.putLong(id);packet.putInt(sx);packet.putInt(sy);packet.putInt(sz);
            if(sy<level.getMinSectionY()||sy>level.getMaxSectionY())resources.empty(packet,Blocks.AIR.defaultBlockState());
            else {
                var chunk=level.getChunkSource().getChunk(sx,sz,ChunkStatus.FULL,false);
                if(chunk==null){for(int word=0;word<4;word++)packet.putInt(0);}
                else resources.section(packet,chunk.getSection(chunk.getSectionIndexFromSectionY(sy)));
            }
            completed++;
            // 压缩复制以一个section为最小单元，未回复ticket由world下帧重放。
            if(System.nanoTime()-started>=BUDGET_NS)break;
        }
        packet.patchInt(4,completed);return packet.finish();
    }

    ByteBuffer tints(Minecraft minecraft,ByteBuffer requests,int count){
        colors.clear();
        for(int i=0;i<count;i++){
            int at=i*24;
            if(requests.getInt(at+20)!=0)throw new IllegalArgumentException("tint保留位无效");
            tintPosition.set(requests.getInt(at),requests.getInt(at+4),requests.getInt(at+8));
            var state=net.minecraft.world.level.block.Block.BLOCK_STATE_REGISTRY.byId(requests.getInt(at+12));
            if(state==null)throw new IllegalStateException("tint请求状态ID未定义");
            int slot=requests.getInt(at+16);
            var source=slot==-1?minecraft.getModelManager().getFluidStateModelSet().get(state.getFluidState()).tintSource():minecraft.getBlockColors().getTintSource(state,slot);
            colors.putInt(source==null?-1:source.colorInWorld(state,level,tintPosition));
        }return colors.finish();
    }
}
