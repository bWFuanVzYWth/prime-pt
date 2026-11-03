package dev.primept;

import java.lang.reflect.Field;
import java.util.List;
import net.minecraft.client.renderer.texture.SpriteContents;

/** 只借用实际帧序列；atlas初始颜色与资源字典使用同一元数据，不按unique集合猜首帧。 */
final class SourceAnimation {
    private static final Field ANIMATION=field(SpriteContents.class,"animatedTexture"),FRAMES=field(ANIMATION.getType(),"frames"),INTERPOLATE=field(ANIMATION.getType(),"interpolateFrames");
    private static final Class<?> FRAME=frameClass();
    private static final Field INDEX=field(FRAME,"index"),TIME=field(FRAME,"time");
    record Metadata(List<?> frames,boolean interpolate){
        int firstFrame(){return frames.isEmpty()?0:index(frames.getFirst());}
    }
    static Metadata metadata(SpriteContents contents){
        Object animation=get(ANIMATION,contents);
        return animation==null?new Metadata(List.of(),false):new Metadata((List<?>)get(FRAMES,animation),(boolean)get(INTERPOLATE,animation));
    }
    static int index(Object frame){return (int)get(INDEX,frame);}
    static int duration(Object frame){return (int)get(TIME,frame);}
    private static Class<?> frameClass(){for(var type:SpriteContents.class.getDeclaredClasses())if(type.getSimpleName().equals("FrameInfo"))return type;throw new ExceptionInInitializerError("SpriteContents.FrameInfo缺失");}
    private static Field field(Class<?> type,String name){try{var field=type.getDeclaredField(name);field.setAccessible(true);return field;}catch(ReflectiveOperationException error){throw new ExceptionInInitializerError(error);}}
    private static Object get(Field field,Object value){try{return field.get(value);}catch(IllegalAccessException error){throw new IllegalStateException("动画源字段不可访问",error);}}
}
