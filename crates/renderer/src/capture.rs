//! 诊断用的真实场景快照。保存渲染入口已经接受的资源和几何，不重新解释 Minecraft 模型。
//!
//! 文件仅供同版本、little-endian 构建回放；不是存档格式。资源按世代单独保存，帧只引用该世代。
//! 写入使用 create_new，失败保留不完整文件供诊断；只有完成标记存在的目录可用于回放。
use super::{Gpu, Options, ResourceUpload};
use prime_scene::{Camera, Environment, RenderView};
use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::Path,
};

const _: () = assert!(cfg!(target_endian = "little"));

fn file(path: &Path) -> io::Result<BufWriter<std::fs::File>> {
    Ok(BufWriter::new(
        OpenOptions::new().write(true).create_new(true).open(path)?,
    ))
}
fn pod<T: bytemuck::Pod>(out: &mut impl Write, value: &T) -> io::Result<()> {
    out.write_all(bytemuck::bytes_of(value))
}
fn blob(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    pod(out, &(bytes.len() as u64))?;
    out.write_all(bytes)
}

/// 保存本世代完整资源输入。目录须已存在；不复制或保留调用者的内存。
///
/// # Errors
/// I/O 失败直接返回，已存在文件不会覆盖。
pub fn write_resources(directory: &Path, source: &ResourceUpload<'_>) -> io::Result<()> {
    let mut out = file(&directory.join(format!("resources-{}.bin", source.generation)))?;
    out.write_all(b"PRASSET1")?;
    pod(&mut out, &source.generation)?;
    pod(&mut out, &source.extent)?;
    for bytes in [
        bytemuck::cast_slice(source.materials),
        bytemuck::cast_slice(source.emission_alias_words),
        bytemuck::cast_slice(source.light_areas),
        source.albedo,
        source.mers,
        source.auxiliary,
    ] {
        blob(&mut out, bytes)?;
    }
    out.flush()
}

impl Gpu {
    /// 保存 CPU 已发布的场景与本帧追踪输入，不读取 GPU，不改变随机序号或历史。
    /// 调用者保证 view 是最近成功提交的完整视图，无尚未提交的颜色覆盖。
    ///
    /// # Errors
    /// 目录须已存在；I/O 失败返回且不覆盖旧快照。
    pub fn capture_frame(
        &self,
        directory: &Path,
        view: RenderView<'_>,
        camera: &Camera,
        environment: Environment,
        extent: [u32; 2],
        options: Options,
    ) -> io::Result<()> {
        let mut out = file(&directory.join("frame.bin"))?;
        out.write_all(b"PRSCENE1")?;
        pod(&mut out, &view.generation)?;
        pod(&mut out, &view.anchor)?;
        for vector in [camera.origin, camera.forward, camera.right, camera.up] {
            pod(&mut out, &vector)?;
        }
        pod(&mut out, &environment.sun_direction)?;
        pod(&mut out, &environment.exposure_ev)?;
        pod(&mut out, &environment.sea_level)?;
        pod(&mut out, &self.initial_medium)?;
        pod(&mut out, &[self.sun_gain, self.block_emission_gain])?;
        pod(&mut out, &extent)?;
        let sample = self
            .pending_absolute
            .map_or(self.absolute_sample, |(next, _)| next);
        pod(
            &mut out,
            &[
                options.bounces,
                options.samples,
                sample,
                self.tuning.history_cap(),
                self.tuning.restir_cv() as u32,
                self.invocation_reorder as u32,
            ],
        )?;
        pod(&mut out, &self.tuning.fresh_probability())?;
        pod(&mut out, &(view.regions.len() as u64))?;
        for region in view.regions {
            pod(&mut out, &region.key)?;
            pod(
                &mut out,
                &[region.geometry_revision, region.content_revision],
            )?;
            pod(&mut out, &(region.mesh.len() as u64))?;
            for quad in region.mesh.quads() {
                pod(&mut out, quad)?;
            }
        }
        out.flush()?;
        let mut complete = file(&directory.join("complete"))?;
        complete.write_all(b"PRSCENE1\n")?;
        complete.flush()
    }
}
