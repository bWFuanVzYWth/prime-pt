package dev.primept;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Path;
import java.util.ArrayList;
import static java.lang.foreign.ValueLayout.*;

/** 真实DLL+生产invokeExact调用点；无效session只用于避免创建客户端/GPU，不能冒充应用成功。 */
final class NativeBatchFixture {
    public static void main(String[] args)throws Throwable{
        try(var arena=Arena.ofConfined()){
            var symbols=SymbolLookup.libraryLookup(Path.of(System.getProperty("primept.native")),arena);var linker=Linker.nativeLinker();
            var batch=linker.downcallHandle(symbols.find("prime_invalidate_batch").orElseThrow(),FunctionDescriptor.of(JAVA_INT,JAVA_LONG,ADDRESS,JAVA_LONG));
            var error=linker.downcallHandle(symbols.find("prime_error").orElseThrow(),FunctionDescriptor.of(ADDRESS));
            var profile=linker.downcallHandle(symbols.find("prime_profile").orElseThrow(),FunctionDescriptor.of(JAVA_INT,JAVA_LONG,JAVA_LONG,ADDRESS,ADDRESS,JAVA_INT,JAVA_INT));
            var stages=ByteBuffer.allocateDirect(FrameChart.STAGES*8).order(ByteOrder.LITTLE_ENDIAN);
            if(Native.invokeProfile(profile,0L,8_000_000L,stages,null,0)!=-1||!message(error).equals("无效 native 句柄"))throw new AssertionError("隐藏图表必须以NULL/0输出通过FFM与指针检查");
            var packets=packets();
            for(var packet:packets){
                int status=Native.invokeInvalidationBatch(batch,0L,packet);
                if(status!=-1||!message(error).equals("无效 native 句柄"))throw new AssertionError("合法PDI1必须通过完整解析并抵达session查找："+message(error));
            }
            var valid=packets.getFirst();
            for(int offset:new int[]{0,4,12,16,32}){
                var malformed=copy(valid);malformed.putInt(offset,0x7fffffff);reject(batch,error,malformed);
            }
            var reversed=copy(valid);reversed.putInt(32,4095);reversed.putInt(36,0);reject(batch,error,reversed);
            reject(batch,error,null);
            System.out.println("PDI1真实FFM：6类生产包通过解析，7类非法包先拒绝；PFT2隐藏NULL/0通过边界；生产调用点正确，无客户端/GPU");
        }
    }
    private static ArrayList<ByteBuffer> packets(){
        Object world=new Object();var out=new ArrayList<ByteBuffer>();
        for(int kind=1;kind<=6;kind++){
            var changes=new SourceChanges(kind==6?2:1024);
            switch(kind){case 1->changes.block(world,-1,15,16);case 2->changes.section(world,-2,-4,5);case 3->changes.column(world,4,5,-4,20,true);case 4->changes.biome(world,-2,3);case 5->changes.all(world);case 6->{changes.section(world,1,0,0);changes.section(world,2,0,0);}default->throw new AssertionError();}
            out.add(copy(changes.batch(world,16)));changes.accepted();
        }
        return out;
    }
    private static ByteBuffer copy(ByteBuffer value){var result=ByteBuffer.allocateDirect(value.remaining()).order(ByteOrder.LITTLE_ENDIAN);result.put(value.duplicate());return result.flip();}
    private static String message(MethodHandle error)throws Throwable{var source=(MemorySegment)error.invokeExact();return source.reinterpret(4096).getString(0);}
    private static void reject(MethodHandle batch,MethodHandle error,ByteBuffer bytes)throws Throwable{
        if(Native.invokeInvalidationBatch(batch,0L,bytes)!=-1||message(error).equals("无效 native 句柄"))throw new AssertionError("非法完整包不得抵达session应用");
    }
}
