//! 世界捕获、候选来源与失效通知；未完成候选不提前发布。

use super::*;

/// 依宿主水平视距及完整世界高度推进64m LOD0窗口，返回至多capacity条待捕请求。
///
/// 请求为 little-endian 的 `id:u64, sx:i32, sy:i32, sz:i32, reserved:u32=0`，每条 24B。
/// view_distance以16m列计；高度为半开section范围。每个region合并4³个section。
/// 未回复请求在后续调用保持同一ID；宿主可分帧用PRT3回复，捕获失败才调用
/// [`prime_fail_capture`]。窗口移动不清空场景，超出联合容量返回错误。
///
/// # Safety
/// camera 借出完整 WorldCamera；requests 借出 capacity*24 字节可写内存，二者不重叠。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_plan(
    id: u64,
    camera: *const WorldCamera,
    view_distance: u32,
    min_section_y: i32,
    max_section_y_exclusive: i32,
    requests: *mut u8,
    capacity: u32,
) -> i32 {
    guarded(-1, || {
        if camera.is_null() || requests.is_null() || capacity == 0 || capacity > 64 {
            return Err(Error::Input("捕获请求指针或容量无效"));
        }
        // SAFETY: 调用者借出完整相机，不要求对齐。
        let camera = unsafe { camera.read_unaligned() };
        let mut count = 0;
        with_live_engine(id, |engine| {
            engine.grow_view_budget(view_distance)?;
            let planned: &[scene::CaptureRequest] = engine.scene.plan(
                &camera,
                view_distance,
                min_section_y,
                max_section_y_exclusive,
                capacity as usize,
            )?;
            for (index, request) in planned.iter().enumerate() {
                let mut bytes = [0u8; 24];
                bytes[..8].copy_from_slice(&request.id.to_le_bytes());
                for axis in 0..3 {
                    bytes[8 + axis * 4..12 + axis * 4]
                        .copy_from_slice(&request.section[axis].to_le_bytes());
                }
                // SAFETY: plan 返回条数不超过 capacity；调用者提供完整可写输出。
                unsafe {
                    requests
                        .add(index * 24)
                        .copy_from_nonoverlapping(bytes.as_ptr(), 24)
                };
            }
            count = planned.len() as i32;
            engine.retire_tint_sources();
            let meshing = std::time::Instant::now();
            let result = engine.scene.compile_sources(&engine.catalog);
            engine.profile.plan_mesh_ns = engine
                .profile
                .plan_mesh_ns
                .saturating_add(elapsed_ns(meshing));
            result?;
            Ok(())
        })?;
        Ok(count)
    })
}

/// 失效一个已保留 section 的捕获源，旧可见资源保留到替换成功；未知 section 忽略。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_section(id: u64, sx: i32, sy: i32, sz: i32) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.tint_pending = None;
            Ok(engine.scene.invalidate_section([sx, sy, sz])?)
        })
    })
}

/// 同步消费PDI1变化批；完整验证后才入场景队列，几何与biome/tint依赖分别失效。
/// # Safety
/// data在调用内借出len字节只读连续内存；调用者独占会话，失败可保留同一包重试。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_invalidate_batch(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("变化包长度越界"))?;
        if data.is_null() || !(16..=scene::MAX_PACKET).contains(&len) {
            return Err(Error::Input("变化包指针或长度无效"));
        }
        // SAFETY: 调用者保持完整借用，上方已校验长度。
        let input = unsafe { std::slice::from_raw_parts(data, len) };
        let changes = changes::decode(input)?;
        with_live_engine(id, |engine| {
            engine.scene.apply_changes(&changes)?;
            engine.tint_pending = None;
            for change in &changes {
                match *change {
                    scene::SourceChange::BiomeColumn { x, z }
                    | scene::SourceChange::ColumnSource {
                        x, z, biome: true, ..
                    } => {
                        engine.tint_columns.insert((x, z));
                    }
                    scene::SourceChange::WorldTint => {
                        if let Some(world) = &mut engine.tint_world {
                            world.invalidate_colors();
                        }
                    }
                    scene::SourceChange::SourceOverflow => {
                        engine.tint_invalidate_all = true;
                        engine.tint_columns.clear();
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    })
}

/// 将全部已知源排入分步刷新；不清空可见场景或 RR 历史。
#[unsafe(no_mangle)]
pub extern "C" fn prime_invalidate_all(id: u64) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| {
            engine.scene.invalidate_all();
            engine.tint_pending = None;
            engine.tint_invalidate_all = true;
            Ok(())
        })
    })
}

/// 标记一次宿主捕获失败；过期请求忽略，仍需要的 section 可在之后重试。
#[unsafe(no_mangle)]
pub extern "C" fn prime_fail_capture(id: u64, request: u64) -> i32 {
    guarded(-1, || {
        with_live_engine(id, |engine| Ok(engine.scene.fail_capture(request)?))
    })
}

/// 同步消费PRT2捕获包；验证失败保留已发布场景，阶段资源按region ticket保留。
///
/// 所有字为 little-endian，坐标为 i32，其余整数为 u32，浮点为 f32：
/// `magic=0x32545250, section_count`；每段为 `request_id:u64,sx,sy,sz,block_count`；
/// 每块为 `x | y<<4 | z<<8, quad_count, offset_x,offset_y,offset_z`；
/// 每 quad 为 `ARGB, [x,y,z,u,v] × 4`。
/// quad 坐标相对方块，UV 对应 atlas；每包 1..=64 段，可跨 region。
/// PRT3保留原始palette/storage，收到必要邻域后由Rust编译；tint通过独立批阶段补齐。
/// 兼容PRT2中block_count=0表示真实空section；u32::MAX表示源chunk未加载。
/// 不可用region保留显式负缓存和已有GPU覆盖，加载通知后重新捕获完整region。
/// 旧ticket回复忽略，未知邻域不猜为空，失败不发布半个region。
///
/// # Safety
/// data 指向 len 字节的只读连续内存，调用期间保持活动且不被其他线程修改。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_batch(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("源包长度越界"))?;
        if data.is_null() || len == 0 || len > scene::MAX_PACKET {
            return Err(Error::Input("源包指针或长度无效"));
        }
        // SAFETY: 调用者保证整段内存只读且在调用内活动；上方限制长度。
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        with_live_engine(id, |engine| {
            if bytes.starts_with(&0x33545250u32.to_le_bytes()) {
                engine.scene.apply_compact(bytes, &engine.catalog)?;
                engine.retire_tint_sources();
            } else {
                engine.scene.apply(bytes)?;
            }
            Ok(())
        })
    })
}
