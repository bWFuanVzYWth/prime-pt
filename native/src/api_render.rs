//! 宿主帧与诊断输出；借用设备/队列的安全前提保留在各入口。

use super::*;

/// 记录完整宿主帧间隔并导出PFT2主线程卡顿快照；成功返回写入字节数，失败返回-1。
///
/// elapsed_ns=0清空滚动窗口并跳过暂停间隔，保留会话最慢记录。
/// stages为14个little-endian u64纳秒：等待、tint/源维护、规划、宿主捕获、网格编译/合并、
/// 几何交接、属性打包、BLAS准备/等待、属性分配/写入、TLAS准备/等待、场景提交/回收、
/// native录制/SDK、宿主提交、未归因。最近场景更新规模按独立序号导出，非所选柱的规模。
/// plan内的同步编译从规划移到网格项；native场景细分从录制总耗时扣除，
/// 视图准备归场景准备/提交，余量由引擎统计补齐；不提供GPU timestamp。
/// columns为1..=512个实际图表像素列，输出容量至少728+120*columns且不超过62168。
/// columns=0时只记录，output须为空且capacity=0；隐藏图表不生成或传输窗口快照。
///
/// # Safety
/// stages借出至少112B只读内存，output借出capacity字节可写内存；二者不重叠，
/// 调用期间不得并发修改。该CPU接口不改变宿主GPU完成约定。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_profile(
    id: u64,
    elapsed_ns: u64,
    stages: *const u8,
    output: *mut u8,
    capacity: u32,
    columns: u32,
) -> i32 {
    guarded(-1, || {
        let columns = columns as usize;
        let capacity = capacity as usize;
        if stages.is_null()
            || columns > diagnostics::COLUMNS
            || (columns == 0 && (!output.is_null() || capacity != 0))
            || (columns != 0
                && (output.is_null()
                    || capacity < diagnostics::HEADER + columns * diagnostics::BAR
                    || capacity > diagnostics::HEADER + diagnostics::COLUMNS * diagnostics::BAR))
        {
            return Err(Error::Input("PFT2指针、容量或柱数无效"));
        }
        let timings = std::array::from_fn(|i| {
            // SAFETY: 调用者借出完整112B；不要求对齐。
            u64::from_le(unsafe { stages.add(i * 8).cast::<u64>().read_unaligned() })
        });
        let mut size = 0;
        with_engine(id, |engine| {
            engine.profile.set_pressure(engine.scene.pressure());
            engine.profile.set_work(
                engine.scene.work_stats(),
                engine.scene.work_budget().mesh_slabs,
            );
            engine.profile.record(elapsed_ns, timings)?;
            if columns != 0 {
                // SAFETY: 调用者独占完整可写输出；非空指针与长度上限均已检查。
                let output = unsafe { std::slice::from_raw_parts_mut(output, capacity) };
                size = engine.profile.write(output, columns)? as i32;
            }
            Ok(())
        })?;
        Ok(size)
    })
}

/// 合批脏场景、构建加速结构并录制引擎 PT 与宿主呈现命令；不提交宿主 command。
///
/// # Safety
/// command 正在录制且不在 render pass 内；image/view 为本设备的 RGBA8_UNORM
/// STORAGE 图像，尺寸匹配，入口/出口布局均为 GENERAL（RenderPearl 26.3 约定）。
/// 输出第 0 行对应宿主画面底部；相机 up 保持世界向上的视锥向量。
/// camera 指向一份活动的 72B [`WorldCamera`]；函数返回后借用结束。
/// 场景和相机统一在 64m 网格 anchor 下计算；重定位保留 BLAS 与 RR 世界空间历史。
/// features 借出 feature_bytes 个 PTF1 字节；其中的 sampled view 属于本设备，在本次提交完成前为 GENERAL 并保活。冻结时可传空。
/// environment 指向一份活动的 16B [`FrameEnvironment`]。曝光只影响呈现。
/// rr=0 使用原始累积，rr=1 使用已完成早期 SDK 接入的 RR；其余值拒绝。
/// quality=0..=4 对应 DLAA/Quality/Balanced/Performance/UltraPerformance；SDK 决定输入尺寸。
/// 实时固定 2 spp，samples=1..=64 仅用于离线批大小。RR 重建本帧样本均值，原始 PT 保留逐帧累积。bounces=1..=64 为最多表面反弹次数，默认12；末顶点保留发光与NEE。
/// [`prime_set_rendering`] 选择离线时固定最近已接受帧的场景、相机、光照和路径预算，
/// 只接受当前曝光、尺寸和每帧采样量；没有已接受帧时先绘制并接受首帧。离线禁用 RR，
/// 离线累积将超过2^24个样本时显式返回错误；由用户重新进入离线模式，不自动丢弃均值。
/// 累积失效不回退独立的绝对随机基数；完整帧在宿主完成边界接受，取消不发布新序号。
/// 宿主已确认前次 draw 完成，并独占 graphics queue；mesh 更新会同步提交构建命令。
/// 返回成功后宿主负责提交该 command，并在任何资源替换/销毁前等待完成。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prime_draw(
    id: u64,
    command: u64,
    image: u64,
    image_view: u64,
    width: u32,
    height: u32,
    camera: *const WorldCamera,
    environment: *const FrameEnvironment,
    rr: u32,
    quality: u32,
    samples: u32,
    bounces: u32,
    features: *const u8,
    feature_bytes: u32,
) -> i32 {
    guarded(-1, || {
        if rr > 3
            || [command, image, image_view].contains(&0)
            || width == 0
            || height == 0
            || camera.is_null()
            || environment.is_null()
        {
            return Err(Error::Input("draw 句柄、尺寸、camera 或 environment 无效"));
        }
        // SAFETY: 调用者借出至少一份 WorldCamera；不要求源指针对齐。
        let source = unsafe { camera.read_unaligned() };
        let source_anchor = source.anchor()?;
        source.relative(source_anchor)?;
        // SAFETY: 调用者借出完整帧环境包，不要求源指针对齐。
        let source_environment = unsafe { environment.read_unaligned() };
        let options = gpu::Options::new(
            rr == 1 || rr == 3,
            streamline::Quality::try_from(quality)?,
            samples,
            bounces,
        )?;
        let options = match rr {
            1 => options.with_nrd_fallback(),
            2 => options.with_nrd(),
            _ => options,
        };
        with_engine(id, |engine| {
            let incoming = source_environment.resolve(engine.solar_orbit, source_anchor[1])?;
            let frozen = engine.rendering.frozen();
            let frame = engine.rendering.choose(rendering::Frame {
                camera: source,
                environment: incoming,
                bounces,
            });
            let anchor = frame.camera.anchor()?;

            let environment = frame.environment;
            let options = if engine.rendering.mode() == prime_scene::RenderMode::Offline {
                gpu::Options::new(
                    false,
                    streamline::Quality::try_from(quality)?,
                    samples,
                    frame.bounces,
                )?
            } else {
                options
            };
            if !engine.atlas_ready {
                return Err(Error::Input("draw 前需要上传 atlas"));
            }
            engine.profile.scene = [0; 7];
            engine.profile.update = None;
            let prepare_started = std::time::Instant::now();
            if !frozen {
                engine.scene.rebase(anchor);
            }
            let changed = !frozen
                && (engine.scene.prepare()?
                    || engine.gpu.scene_resources_dirty()
                    || engine.gpu.scene_anchor() != engine.scene.anchor()
                    || engine.gpu.terrain_pending());
            engine.profile.scene[0] = elapsed_ns(prepare_started);
            if changed {
                let scene_started = std::time::Instant::now();
                let publication = (|| {
                    let changes: Vec<_> = engine.scene.render_changes().collect();
                    if engine.gpu.try_attribute_changes(
                        engine.scene.generation(),
                        engine.scene.anchor(),
                        &changes,
                    )? {
                        return Ok(true);
                    }
                    let regions: Vec<_> = engine.scene.render_regions().collect();
                    engine.gpu.stream_regions(prime_scene::RenderView {
                        generation: engine.scene.generation(),
                        regions: &regions,
                        anchor: engine.scene.anchor(),
                    })
                })();
                match publication {
                    Ok(true) => engine.scene.commit()?,
                    Ok(false) => engine.scene.park_publication(),
                    Err(error) => {
                        engine.scene.reject()?;
                        return Err(error.into());
                    }
                }
                let timing = engine.gpu.scene_timing().expect("成功发布保存当前计时");
                engine.profile.scene[1] = timing
                    .geometry_ns
                    .saturating_sub(timing.attribute_pack_ns + timing.light_pack_ns);
                engine.profile.scene[2] = timing.attribute_pack_ns + timing.light_pack_ns;
                engine.profile.scene[3] = timing.acceleration_ns.saturating_sub(timing.tlas_ns);
                engine.profile.scene[4] = timing.attribute_upload_ns + timing.light_upload_ns;
                engine.profile.scene[5] = timing.tlas_ns;
                engine.profile.scene[6] = elapsed_ns(scene_started).saturating_sub(
                    timing
                        .geometry_ns
                        .saturating_add(timing.acceleration_ns)
                        .saturating_add(timing.attribute_upload_ns + timing.light_upload_ns),
                );
                engine.profile.update = Some(diagnostics::SceneInfo {
                    dirty_regions: timing.dirty_regions as u64,
                    instances: timing.instances as u64,
                    terrain_upload_bytes: timing.terrain_upload_bytes,
                    device_copy_bytes: timing.attribute_copy_bytes,
                    as_wait_ns: timing.as_submit_wait_ns,
                    tlas_ns: timing.tlas_ns,
                    attribute_ns: timing.attribute_upload_ns + timing.light_upload_ns,
                });
            }
            if engine.rendering.mode() == prime_scene::RenderMode::Offline
                && engine.gpu.accumulated_samples() > (1 << 24) - samples
            {
                return Err(Error::Input(
                    "离线样本达到精确累积上限；请显式重新进入离线模式",
                ));
            }
            if feature_bytes != 0 {
                if features.is_null() || feature_bytes as usize > scene::MAX_PACKET {
                    return Err(Error::Input("动态网格源包指针或大小无效"));
                }
                // SAFETY: 本入口借用完整 PTF1 和同设备纹理；设备、布局和保活由宿主维持。
                unsafe {
                    engine.gpu.set_world_features(std::slice::from_raw_parts(
                        features,
                        feature_bytes as usize,
                    ))?;
                }
            }
            let published_anchor = engine.gpu.scene_anchor();
            let camera = frame.camera.relative(published_anchor)?;
            if engine.capture.as_mut().is_some_and(capture::Capture::frame) {
                let capture = engine.capture.take().expect("捕获计数到达零");
                let regions: Vec<_> = engine.scene.render_regions().collect();
                engine
                    .gpu
                    .capture_frame(
                        &capture.directory,
                        prime_scene::RenderView {
                            generation: engine.scene.generation(),
                            regions: &regions,
                            anchor: published_anchor,
                        },
                        &camera,
                        environment,
                        [width, height],
                        options,
                    )
                    .map_err(gpu::Error::DiagnosticOutput)?;
                eprintln!(
                    "[prime_pt] scene_capture complete path={} regions={}",
                    capture.directory.display(),
                    regions.len()
                );
            }
            engine.gpu.draw(
                command,
                image,
                image_view,
                [width, height],
                &camera,
                environment,
                options,
            )?;
            engine.rendering.accepted(frame);
            Ok(())
        })
    })
}
