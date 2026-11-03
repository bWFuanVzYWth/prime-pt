package dev.primept;

import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.Random;
import net.minecraft.world.level.levelgen.LegacyRandomSource;

/** 实际26.3模型随机源，验证nextInt rejection后的流位置及nextLong有符号拼接。 */
final class ModelRandomFixture {
    public static void main(String[] arguments)throws Exception{
        var seeds=new ArrayList<Long>();
        for(long seed:new long[]{0,1,-1,Long.MIN_VALUE,Long.MAX_VALUE,0x5deece66dL,0x123456789abcdefL})seeds.add(seed);
        var source=new Random(0x31ea9c82);for(int i=0;i<32;i++)seeds.add(source.nextLong());
        int[] bounds={1,2,3,7,16,31,1073741825,Integer.MAX_VALUE};
        var out=new Packet();out.putInt(0x31474e52);out.putInt(263);out.putInt(seeds.size());
        for(long seed:seeds){
            var random=new LegacyRandomSource(seed);int integers=0;out.putLong(seed);out.putInt(96);
            for(int operation=0;operation<96;operation++){
                if(operation%3==0){out.putInt(0);out.putInt(0);out.putLong(random.nextLong());}
                else{int bound=bounds[integers++%bounds.length];out.putInt(1);out.putInt(bound);out.putLong(random.nextInt(bound));}
            }
        }
        Path path=Path.of(arguments[0]);Files.createDirectories(path.toAbsolutePath().getParent());
        try(var channel=FileChannel.open(path,StandardOpenOption.CREATE,StandardOpenOption.TRUNCATE_EXISTING,StandardOpenOption.WRITE)){var bytes=out.finish();while(bytes.hasRemaining())channel.write(bytes);}
        System.out.println("实际26.3 LegacyRandomSource oracle: "+seeds.size()+" seeds x96 continuous operations -> "+path.toAbsolutePath());
    }
}
