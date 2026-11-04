package dev.primept;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.IdentityHashMap;
import net.minecraft.core.BlockPos;
import net.minecraft.world.level.block.DoorBlock;
import net.minecraft.world.level.block.DoublePlantBlock;
import net.minecraft.world.level.block.SmallDripleafBlock;
import net.minecraft.world.level.block.SpeleothemBlock;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.BedPart;
import net.minecraft.world.level.block.state.properties.BlockStateProperties;
import net.minecraft.world.level.block.state.properties.DoubleBlockHalf;

/** 只捕获位置规则声明；offset与seed的逐位置求值归Rust。 */
final class SourcePlacement {
    private static final SourcePlacement INSTANCE = new SourcePlacement(net.minecraft.world.level.block.AbstractBedBlock.class);
    private static final Field OFFSET =
            field(BlockBehaviour.BlockStateBase.class, "offsetFunction");
    private static final Field PROPERTY_OFFSET =
            field(BlockBehaviour.Properties.class, "offsetFunction");
    private static final Class<?> XZ = standardOffset(BlockBehaviour.OffsetType.XZ),
                                  XYZ = standardOffset(BlockBehaviour.OffsetType.XYZ);
    private final Class<?> bedSeedDeclaration;
    private final IdentityHashMap<Class<?>, Methods> methods = new IdentityHashMap<>();

    SourcePlacement(Class<?> bedSeedDeclaration) {
        this.bedSeedDeclaration = bedSeedDeclaration;
    }
    record Placement(int kind, float horizontal, float vertical, int seed) {
        boolean hasOffset() {
            return kind != 0;
        }
    }
    static Placement prepare(BlockState state) { return INSTANCE.describe(state); }
    private Placement describe(BlockState state) {
        var block = state.getBlock();
        var binding = methods.computeIfAbsent(block.getClass(), Methods::new);
        Object offset = get(OFFSET, state);
        int kind = offset == null             ? 0
                   : offset.getClass() == XZ  ? 1
                   : offset.getClass() == XYZ ? 2
                                              : 3;
        float horizontal = 0, vertical = 0;
        if (kind == 1 || kind == 2) {
            var h = binding.horizontal.getDeclaringClass();
            var v = binding.vertical.getDeclaringClass();
            if ((h != BlockBehaviour.class && h != SpeleothemBlock.class) ||
                (kind == 2 && v != BlockBehaviour.class && v != SmallDripleafBlock.class)) {
                kind = 3;
            } else {
                // 已知原版声明返回不可变常量，不执行未知override来猜测位置规则。
                horizontal = number(binding.horizontal, block);
                if (kind == 2)
                    vertical = number(binding.vertical, block);
            }
        }
        var seedOwner = binding.seed.getDeclaringClass();
        int seed;
        if (seedOwner == BlockBehaviour.class) {
            seed = 0;
        } else if (seedOwner == DoublePlantBlock.class || seedOwner == DoorBlock.class) {
            seed = state.getValue(BlockStateProperties.DOUBLE_BLOCK_HALF) == DoubleBlockHalf.UPPER
                           ? 1
                           : 0;
        } else if (seedOwner == bedSeedDeclaration) {
            seed = state.getValue(BlockStateProperties.BED_PART) == BedPart.HEAD
                           ? 0
                           : switch (state.getValue(BlockStateProperties.HORIZONTAL_FACING)) {
                                 case NORTH -> 2;
                                 case SOUTH -> 3;
                                 case WEST -> 4;
                                 case EAST -> 5;
                                 default ->
                                     throw new IllegalStateException(
                                             "Non-horizontal bed source facing");
                             };
        } else {
            seed = 6;
        }
        return new Placement(kind, horizontal, vertical, seed);
    }
    private static final class Methods {
        final Method horizontal, vertical, seed;
        Methods(Class<?> type) {
            horizontal = method(type, "getMaxHorizontalOffset");
            vertical = method(type, "getMaxVerticalOffset");
            seed = method(type, "getSeed", BlockState.class, BlockPos.class);
        }
    }
    private static Class<?> standardOffset(BlockBehaviour.OffsetType type) {
        // 用实际无捕获lambda类识别标准实现，不求值位置offset或构造方块状态。
        return get(PROPERTY_OFFSET, BlockBehaviour.Properties.of().offsetType(type)).getClass();
    }
    private static float number(Method method, Object source) {
        try {
            return (float)method.invoke(source);
        } catch (ReflectiveOperationException error) {
            throw new IllegalStateException("Cannot read standard placement source", error);
        }
    }
    private static Method method(Class<?> type, String name, Class<?>... arguments) {
        for (Class<?> owner = type; owner != null; owner = owner.getSuperclass()) {
            try {
                var method = owner.getDeclaredMethod(name, arguments);
                method.setAccessible(true);
                return method;
            } catch (NoSuchMethodException absent) {
                // 继续查找实际继承声明，不根据类型名称猜测。
            }
        }
        throw new IllegalStateException("Missing placement source declaration: " + name);
    }
    private static Field field(Class<?> type, String name) {
        try {
            var field = type.getDeclaredField(name);
            field.setAccessible(true);
            return field;
        } catch (ReflectiveOperationException error) {
            throw new ExceptionInInitializerError(error);
        }
    }
    private static Object get(Field field, Object source) {
        try {
            return field.get(source);
        } catch (IllegalAccessException error) {
            throw new IllegalStateException(error);
        }
    }
}
