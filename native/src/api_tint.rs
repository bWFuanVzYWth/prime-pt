//! 生物群系与颜色的分批捕获；显式续传由Engine持有。

use super::*;

/// 纯tint/biome缓存失效，不丢弃palette/storage或重新准备全局模型资源。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_tints(id: u64) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.scene.invalidate_tints();
            engine.tint_pending = None;
            if let Some(world) = &mut engine.tint_world {
                world.invalidate_colors();
            }
            Ok(())
        })
    })
}

/// 接受当前世界完整PBW1原始biome调色板与seed；失败保留旧世界tint所有者。
/// # Safety
/// data借出len个只读字节到返回，调用者独占本会话。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_biome_world(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("biome包长度越界"))?;
        if data.is_null() || len == 0 || len > prime_minecraft::MAX_PACKET {
            return Err(Error::Input("biome包指针或长度无效"));
        }
        // SAFETY: 长度已校验，调用者保持完整只读借用。
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        with_live_engine(id, |engine| {
            let resources = engine
                .catalog
                .tints()
                .ok_or(Error::Input("世界biome源前需要tint资源"))?;
            let world = prime_minecraft::tint::TintWorld::decode(bytes, resources)?;
            engine.tint_world = Some(world);
            engine.tint_pending = None;
            engine.tint_radius = None;
            engine.tint_columns.clear();
            engine.tint_invalidate_all = false;
            Ok(())
        })
    })
}

/// 完成纯Rust原版tint计划；缺少的quart与自定义回调由两个独立批阶段补齐。
#[unsafe(no_mangle)]
pub extern "C" fn prime_tint_begin(id: u64, radius: u32) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            if radius > 7 {
                return Err(Error::Input("tint半径须0..=7"));
            }
            engine.tint_pending = None;
            if engine.tint_radius.is_some_and(|old| old != radius) {
                engine.scene.invalidate_tints();
            }
            engine.tint_radius = Some(radius);
            if let Some(world) = engine.tint_world.as_mut()
                && (engine.tint_invalidate_all || !engine.tint_columns.is_empty())
            {
                world.invalidate(engine.tint_invalidate_all, &engine.tint_columns);
                engine.tint_columns.clear();
                engine.tint_invalidate_all = false;
            }
            let requests: Vec<_> = engine.scene.tint_requests().collect();
            if requests.is_empty() {
                return Ok(());
            }
            let resources = engine
                .catalog
                .tints()
                .ok_or(Error::Input("缺少全量tint recipe"))?;
            let world = engine
                .tint_world
                .as_mut()
                .ok_or(Error::Input("biome世界源尚未准备"))?;
            let plan = world.plan(&requests, resources, radius)?;
            let ids = plan.biome_requests().is_empty().then(Vec::new);
            let host = plan.host_requests().is_empty().then(Vec::new);
            engine.tint_pending = Some(TintContinuation { plan, ids, host });
            engine.finish_tints()
        })
    })
}

/// 复制原始quart请求，每条24B为sectionXYZ/reserved0/mask；capacity0查询页数。
/// # Safety
/// 非零capacity时output借出至少capacity*24个可写字节到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_biome_requests(id: u64, output: *mut u8, capacity: u32) -> i32 {
    guarded(-1, || {
        let mut count = 0;
        with_engine(id, |engine| {
            let Some(pending) = &engine.tint_pending else {
                return Ok(());
            };
            let requests = pending.plan.biome_requests();
            count = i32::try_from(requests.len()).map_err(|_| Error::Input("biome请求数量越界"))?;
            if capacity == 0 {
                return Ok(());
            }
            if output.is_null() || (capacity as usize) < requests.len() {
                return Err(Error::Input("biome请求输出容量不足"));
            }
            for (index, request) in requests.iter().enumerate() {
                let mut bytes = [0u8; 24];
                for axis in 0..3 {
                    bytes[axis * 4..axis * 4 + 4]
                        .copy_from_slice(&request.section[axis].to_le_bytes());
                }
                bytes[16..24].copy_from_slice(&request.mask.to_le_bytes());
                // SAFETY: 完整24B输出范围已校验，所有写入相互独立。
                unsafe {
                    output
                        .add(index * 24)
                        .copy_from_nonoverlapping(bytes.as_ptr(), 24);
                }
            }
            Ok(())
        })?;
        Ok(count)
    })
}

/// 完整回填quart请求的原始biome行号，按各mask升位顺序；不由Java求最终颜色。
/// # Safety
/// ids借出count个小端u32；允许未对齐，到返回前保持只读活动。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_biome_samples(id: u64, ids: *const u32, count: u32) -> i32 {
    guarded(-1, || {
        if ids.is_null() && count != 0 {
            return Err(Error::Input("biome响应指针为空"));
        }
        with_live_engine(id, |engine| {
            let pending = engine
                .tint_pending
                .as_mut()
                .ok_or(Error::Input("没有待续接biome计划"))?;
            if pending.plan.biome_sample_count() != count as usize || pending.ids.is_some() {
                return Err(Error::Input("biome响应数量不匹配或重复"));
            }
            let mut values = Vec::new();
            values
                .try_reserve_exact(count as usize)
                .map_err(|_| Error::Input("biome响应容量不足"))?;
            for index in 0..count as usize {
                // SAFETY: 调用者借出完整u32数组，上方校验数量；指针可未对齐。
                values.push(u32::from_le(unsafe { ids.add(index).read_unaligned() }));
            }
            pending.ids = Some(values);
            engine.finish_tints()
        })
    })
}

/// 复制唯一必要tint请求，每条24B为positionXYZ/state/slot/reserved0。
/// capacity=0只查询数量；在回填前宿主不得插入新源/资源世代修改。
///
/// # Safety
/// capacity非零时output指向至少capacity*24个可写字节；借用只持续本调用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_tint_requests(id: u64, output: *mut u8, capacity: u32) -> i32 {
    guarded(-1, || {
        let mut result = 0;
        with_engine(id, |engine| {
            let requests = engine
                .tint_pending
                .as_ref()
                .map_or(&[][..], |pending| pending.plan.host_requests());
            result = i32::try_from(requests.len()).map_err(|_| Error::Input("tint请求数量越界"))?;
            if capacity == 0 {
                return Ok(());
            }
            if output.is_null() || (capacity as usize) < requests.len() {
                return Err(Error::Input("tint输出指针或容量无效"));
            }
            for (index, request) in requests.iter().enumerate() {
                let mut bytes = [0u8; 24];
                for axis in 0..3 {
                    bytes[axis * 4..axis * 4 + 4]
                        .copy_from_slice(&request.position[axis].to_le_bytes());
                }
                bytes[12..16].copy_from_slice(&request.state.to_le_bytes());
                bytes[16..20].copy_from_slice(&request.slot.to_le_bytes());
                // SAFETY: 数量和完整24B输出容量已校验，源输出不重叠。
                unsafe {
                    output
                        .add(index * 24)
                        .copy_from_nonoverlapping(bytes.as_ptr(), 24);
                }
            }
            Ok(())
        })?;
        Ok(result)
    })
}

/// 完整回填上次同步请求的真实ARGB，不接受数量不足的前缀或隐式白色。
///
/// # Safety
/// colors借出count个小端u32，允许未对齐；调用内不被修改，不夹入其他world变更。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_tints(id: u64, colors: *const u32, count: u32) -> i32 {
    guarded(-1, || {
        if colors.is_null() && count != 0 {
            return Err(Error::Input("tint颜色指针为空"));
        }
        with_live_engine(id, |engine| {
            let pending = engine
                .tint_pending
                .as_mut()
                .ok_or(Error::Input("没有待续接host tint计划"))?;
            if pending.plan.host_requests().len() != count as usize || pending.host.is_some() {
                return Err(Error::Input("tint响应数量不匹配或重复"));
            }
            let mut values = Vec::new();
            values
                .try_reserve_exact(count as usize)
                .map_err(|_| Error::Input("tint响应容量不足"))?;
            for index in 0..count as usize {
                // SAFETY: 调用者借出全部u32；不要求指针对齐。
                values.push(u32::from_le(unsafe { colors.add(index).read_unaligned() }));
            }
            pending.host = Some(values);
            engine.finish_tints()
        })
    })
}
