//! 完整资源世代输入；校验和准备完成后一次发布。

use super::*;

/// 暂存下一代RGBA8 atlas；不改变当前可见GPU域，后续prime_sources统一准备并发布。
///
/// # Safety
/// pixels 指向 len 字节的只读连续内存。宿主已提交并确认前次 draw 完成，
/// 当前线程独占本会话；此调用只复制借用像素，GPU提交发生在prime_sources。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_atlas(
    id: u64,
    pixels: *const u8,
    len: u64,
    width: u32,
    height: u32,
) -> i32 {
    guarded(-1, || {
        let expected = u64::from(width) * u64::from(height) * 4;
        if pixels.is_null()
            || width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || expected != len
            || len > scene::MAX_PACKET as u64
        {
            return Err(Error::Input(
                "atlas 需要非空 RGBA8，长度匹配且不超过 256 MiB",
            ));
        }
        // SAFETY: 调用者保证在调用内借出的完整 atlas 内存；长度已验证。
        let bytes = unsafe { std::slice::from_raw_parts(pixels, len as usize) };
        with_live_engine(id, |engine| {
            let mut pixels = Vec::new();
            pixels
                .try_reserve_exact(bytes.len())
                .map_err(|_| Error::Input("待准备albedo容量不足"))?;
            pixels.extend_from_slice(bytes);
            engine.pending_albedo = Some(([width, height], pixels));
            Ok(())
        })
    })
}

/// 首次加载/重载的完整PRM1资源事务；失败保留旧Catalog、世界和GPU资源域。
/// 候选shader/SBT/材质/alias全部就绪后才发布，运行期section不得调用本入口追加资源。
///
/// # Safety
/// data在调用内借出完整len字节；宿主已完成此前命令且独占graphics queue。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_sources(id: u64, data: *const u8, len: u64) -> i32 {
    guarded(-1, || {
        let len = usize::try_from(len).map_err(|_| Error::Input("资源包长度越界"))?;
        if data.is_null() || len == 0 || len > prime_minecraft::MAX_PACKET {
            return Err(Error::Input("资源包指针或长度无效"));
        }
        // SAFETY: 调用者保持源包只读活动，上方校验了完整长度。
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        with_live_engine(id, |engine| {
            let (extent, albedo) = engine
                .pending_albedo
                .as_ref()
                .ok_or(Error::Input("源资源准备前需要stage albedo"))?;
            let mut prepared = engine.catalog.stage_resources(bytes)?;
            let generation = prepared.generation();
            let world = engine.scene.stage_source_generation(generation)?;
            {
                let materials = prepared.material_resources(*extent, albedo)?;
                let source = gpu::ResourceUpload {
                    generation,
                    extent: *extent,
                    albedo,
                    materials: materials.materials,
                    mers: &materials.mers,
                    auxiliary: &materials.auxiliary,
                    emission_alias_words: &materials.emission_alias_words,
                    opacity_quads: &materials.opacity_quads,
                    light_areas: &materials.quad_areas,
                };
                if let Some(capture) = &engine.capture {
                    gpu::capture::write_resources(&capture.directory, &source)
                        .map_err(gpu::Error::DiagnosticOutput)?;
                }
                engine.gpu.replace_resources(source)?;
            }
            prepared.commit();
            world.commit();
            engine.tint_world = None;
            engine.tint_pending = None;
            engine.tint_radius = None;
            engine.tint_columns.clear();
            engine.tint_invalidate_all = false;
            engine.pending_albedo = None;
            engine.atlas_ready = true;
            Ok(())
        })
    })
}
