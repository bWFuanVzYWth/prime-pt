//! 真正捕获的世界在无窗口设备上回放，统计单帧 HDR 极值及其时间增长。
use super::*;
use prime_scene::{MaterialDefinition, RenderMode, RenderRegion, RenderView};
use std::{
    fs::{self, File},
    io::{self, BufReader, Read},
    path::Path,
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Reader {
    file: BufReader<File>,
    left: u64,
}

#[test]
#[ignore = "验证实际场景捕获及回放入口的资源和HDR输入；不启动Minecraft"]
fn hardware_scene_capture_fixture() -> Result {
    let root =
        std::env::var_os("PRIME_PT_CAPTURE_FIXTURE").ok_or("缺少 PRIME_PT_CAPTURE_FIXTURE")?;
    let root = Path::new(&root);
    fs::create_dir(root)?;
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let mut material = MaterialDefinition::default();
    material.control[2] = 0;
    material.base_roughness = [0.7, 0.5, 0.3, 0.6];
    let materials = [material];
    let source = ResourceUpload {
        generation: 1,
        extent: [1, 1],
        albedo: &[255; 4],
        materials: &materials,
        mers: &[0, 0, 128, 0],
        auxiliary: &[128, 128, 128, 128],
        emission_alias_words: &[0; 4],
        opacity_quads: &[],
        light_areas: &[[u32::MAX, 0, 0, 0]],
    };
    crate::capture::write_resources(root, &source)?;
    gpu.replace_resources(source)?;
    let mesh = Mesh {
        quads: vec![Quad::with_material(
            [
                [-10., -10., 3.],
                [-10., 10., 3.],
                [10., 10., 3.],
                [10., -10., 3.],
            ],
            [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
            [0.8, 0.9, 1.],
            prime_scene::MaterialId::new(0),
        )],
    };
    let regions = [RenderRegion {
        key: [0; 3],
        geometry_revision: 1,
        content_revision: 1,
        mesh: (&mesh).into(),
    }];
    let view = RenderView {
        generation: 1,
        anchor: [0.; 3],
        regions: &regions,
    };
    gpu.set_regions(view)?;
    gpu.set_render_tuning(prime_scene::RenderTuning::new(true))?;
    let environment = Environment {
        sun_direction: [0., 0., -1.],
        ..Default::default()
    };
    let options = Options::new(false, streamline::Quality::Dlaa, 1, 12)?;
    let extent = [3840, 2160];
    gpu.capture_frame(root, view, &camera(), environment, extent, options)?;
    let image = Image::new(
        &mut gpu.context,
        [extent[0], extent[1], 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(image.into());
    host.render_options(gpu, &image, &camera(), environment, options)?;
    let hdr = gpu.view.as_ref().unwrap().output;
    let readback = Buffer::new(&mut gpu.context, hdr.len(), true)?;
    gpu.test_roots.push(readback.into());
    hdr.copy_prefix_to(&mut gpu.context, &readback, hdr.len())?;
    let mut bytes = vec![0; hdr.len() as usize];
    readback.read(&mut gpu.context, 0, &mut bytes)?;
    fs::write(root.join("expected.rgba32fle"), &bytes)?;
    session.finish()?;
    assert_eq!(host.errors.load(Ordering::Relaxed), 0);
    Ok(())
}
impl Reader {
    fn open(path: &Path, magic: &[u8; 8]) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = Self {
            left: file.metadata()?.len(),
            file: BufReader::new(file),
        };
        let found: [u8; 8] = reader.pod()?;
        if &found != magic {
            return Err(io::Error::other("快照版本不匹配"));
        }
        Ok(reader)
    }
    fn bytes(&mut self, bytes: &mut [u8]) -> io::Result<()> {
        self.left = self
            .left
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("快照长度越界"))?;
        self.file.read_exact(bytes)
    }
    fn pod<T: bytemuck::Pod>(&mut self) -> io::Result<T> {
        let mut value = T::zeroed();
        self.bytes(bytemuck::bytes_of_mut(&mut value))?;
        Ok(value)
    }
    fn elements<T: bytemuck::Pod>(&mut self, count: u64) -> io::Result<Vec<T>> {
        let bytes = count
            .checked_mul(size_of::<T>() as u64)
            .ok_or_else(|| io::Error::other("元素长度溢出"))?;
        if bytes > self.left {
            return Err(io::Error::other("元素超出文件长度"));
        }
        let count = usize::try_from(count).map_err(io::Error::other)?;
        let mut values = Vec::new();
        values.try_reserve_exact(count).map_err(io::Error::other)?;
        values.resize(count, T::zeroed());
        self.bytes(bytemuck::cast_slice_mut(&mut values))?;
        Ok(values)
    }
    fn blob<T: bytemuck::Pod>(&mut self) -> io::Result<Vec<T>> {
        let size: u64 = self.pod()?;
        if !size.is_multiple_of(size_of::<T>() as u64) {
            return Err(io::Error::other("元素未对齐"));
        }
        self.elements(size / size_of::<T>() as u64)
    }
    fn finish(self) -> io::Result<()> {
        if self.left != 0 {
            return Err(io::Error::other("快照存在未解释数据"));
        }
        Ok(())
    }
}

#[test]
#[ignore = "PRIME_PT_REPLAY 指定实际游戏捕获目录；4K 无窗口 PT HDR 回放"]
fn hardware_world_capture_replay() -> Result {
    let root = std::env::var_os("PRIME_PT_REPLAY").ok_or("缺少 PRIME_PT_REPLAY")?;
    let root = Path::new(&root);
    if fs::read(root.join("complete"))? != b"PRSCENE1\n" {
        return Err("快照未完成".into());
    }
    let mut frame = Reader::open(&root.join("frame.bin"), b"PRSCENE1")?;
    let generation: u64 = frame.pod()?;
    let anchor: [f64; 3] = frame.pod()?;
    let camera = Camera {
        origin: frame.pod()?,
        forward: frame.pod()?,
        right: frame.pod()?,
        up: frame.pod()?,
    };
    let environment = Environment {
        sun_direction: frame.pod()?,
        exposure_ev: frame.pod()?,
        sea_level: frame.pod()?,
    };
    let medium: [f32; 4] = frame.pod()?;
    let gains: [f32; 2] = frame.pod()?;
    let captured_extent: [u32; 2] = frame.pod()?;
    if captured_extent.contains(&0) {
        return Err("捕获尺寸不能为零".into());
    }
    let [bounces, _, sample_start, _, _, captured_ser]: [u32; 6] = frame.pod()?;
    let _reserved: f32 = frame.pod()?;
    let count: u64 = frame.pod()?;
    let mut meshes = Vec::new();
    for _ in 0..count {
        let key: [i32; 3] = frame.pod()?;
        let revisions: [u64; 2] = frame.pod()?;
        let quads: u64 = frame.pod()?;
        meshes.push((
            key,
            revisions,
            Mesh {
                quads: frame.elements::<Quad>(quads)?,
            },
        ));
    }
    frame.finish()?;
    let resource_path = root.join(format!("resources-{generation}.bin"));
    let mut magic = [0; 8];
    File::open(&resource_path)?.read_exact(&mut magic)?;
    if magic != *b"PRASSET1" && magic != *b"PRASSET2" {
        return Err("资源格式无效".into());
    }
    let mut resources = Reader::open(&resource_path, &magic)?;
    if resources.pod::<u64>()? != generation {
        return Err("资源世代不匹配".into());
    }
    let atlas_extent: [u32; 2] = resources.pod()?;
    let materials = resources.blob::<MaterialDefinition>()?;
    let alias = resources.blob::<u32>()?;
    let areas = resources.blob::<[u32; 4]>()?;
    let albedo = resources.blob::<u8>()?;
    let mers = resources.blob::<u8>()?;
    let auxiliary = resources.blob::<u8>()?;
    let mut opacity_quads = Vec::new();
    if magic == *b"PRASSET2" {
        let count: u64 = resources.pod()?;
        if count > resources.left / 36 {
            return Err("OMM 资源原型数量超出文件".into());
        }
        opacity_quads.try_reserve_exact(usize::try_from(count)?)?;
        for _ in 0..count {
            opacity_quads.push(simple_engine::opacity::Quad {
                material: resources.pod()?,
                uv: resources.pod()?,
            });
        }
    }
    resources.finish()?;
    let host = Host::new_ser()?;
    let mut session = host.gpu()?;
    let gpu = session.gpu();
    let ser: u32 = std::env::var("PRIME_PT_REPLAY_SER")
        .unwrap_or_else(|_| captured_ser.to_string())
        .parse()?;
    if ser > 1 {
        return Err("PRIME_PT_REPLAY_SER 须为 0/1".into());
    }
    gpu.set_ser(ser != 0)?;
    gpu.replace_resources(ResourceUpload {
        generation,
        extent: atlas_extent,
        materials: &materials,
        albedo: &albedo,
        mers: &mers,
        auxiliary: &auxiliary,
        emission_alias_words: &alias,
        opacity_quads: &opacity_quads,
        light_areas: &areas,
    })?;
    let regions: Vec<_> = meshes
        .iter()
        .map(|(key, revisions, mesh)| RenderRegion {
            key: *key,
            geometry_revision: revisions[0],
            content_revision: revisions[1],
            mesh: mesh.into(),
        })
        .collect();
    gpu.set_regions(RenderView {
        generation,
        anchor,
        regions: &regions,
    })?;
    gpu.set_initial_medium(medium)?;
    gpu.set_lighting_options(gains[0].log2(), gains[1].log2())?;
    let width: u32 = std::env::var("PRIME_PT_REPLAY_WIDTH")
        .unwrap_or_else(|_| "3840".into())
        .parse()?;
    let height =
        (u64::from(width) * u64::from(captured_extent[1]) / u64::from(captured_extent[0])) as u32;
    let extent = [width, height];
    if width == 0 || height == 0 || width > 7680 || height > 4320 {
        return Err("回放尺寸越界".into());
    }
    let frames: u32 = std::env::var("PRIME_PT_REPLAY_FRAMES")
        .unwrap_or_else(|_| "128".into())
        .parse()?;
    if frames == 0 {
        return Err("回放至少一帧".into());
    }
    let image = Image::new(
        &mut gpu.context,
        [width, height, 1],
        ImageFormat::Rgba8Unorm,
    )?;
    gpu.test_roots.push(image.into());
    let options = Options::new(false, streamline::Quality::Dlaa, 1, bounces)?;
    let out =
        root.join(std::env::var("PRIME_PT_REPLAY_OUTPUT").unwrap_or_else(|_| "replay".into()));
    fs::create_dir(&out)?;
    let mut reports = Vec::new();
    let selected = std::env::var("PRIME_PT_REPLAY_CASE").ok();
    for label in ["pt"] {
        if selected.as_deref().is_some_and(|value| value != label) {
            continue;
        }
        gpu.set_rendering(RenderMode::Realtime)?;
        gpu.set_regions(RenderView {
            generation,
            anchor,
            regions: &regions,
        })?;
        gpu.set_render_tuning(prime_scene::RenderTuning::new(true))?;
        gpu.invalidate_history(GlobalReset::RenderDomainChanged)?;
        gpu.absolute_sample = sample_start;
        let mut maxima = Vec::new();
        let mut peak = 0f32;
        let mut mean = [0f64; 3];
        let mut band_mean = [[0f64; 3]; 16];
        let mut readback = None;
        let mut bytes = Vec::new();
        for index in 0..frames {
            host.render_options(gpu, &image, &camera, environment, options)?;
            let hdr = gpu.view.as_ref().unwrap().output;
            let buffer = match readback {
                Some(buffer) => buffer,
                None => {
                    let buffer = Buffer::new(&mut gpu.context, hdr.len(), true)?;
                    gpu.test_roots.push(buffer.into());
                    readback = Some(buffer);
                    bytes.resize(hdr.len() as usize, 0);
                    buffer
                }
            };
            hdr.copy_prefix_to(&mut gpu.context, &buffer, hdr.len())?;
            buffer.read(&mut gpu.context, 0, &mut bytes)?;
            let expected = root.join("expected.rgba32fle");
            if label == "pt" && index == 0 && expected.is_file() {
                assert_eq!(
                    bytes,
                    fs::read(expected)?,
                    "捕获回放改变了同一样本的HDR结果"
                );
            }
            let mut maximum = (0., 0usize, [0.; 3]);
            let mut bright = [0u64; 4];
            let mut frame_mean = [0f64; 3];
            for (pixel, raw) in bytes.as_chunks::<16>().0.iter().enumerate() {
                let rgb: [f32; 3] = std::array::from_fn(|i| {
                    f32::from_le_bytes(raw[4 * i..4 * i + 4].try_into().unwrap())
                });
                if rgb.iter().any(|x| !x.is_finite()) {
                    return Err(
                        format!("{label} frame={index} pixel={pixel} 非有限 {rgb:?}").into(),
                    );
                }
                let value = rgb.into_iter().fold(0., f32::max);
                for (slot, limit) in [1e2, 1e4, 1e6, 1e8].into_iter().enumerate() {
                    bright[slot] += u64::from(value > limit);
                }
                for channel in 0..3 {
                    mean[channel] += f64::from(rgb[channel]);
                    frame_mean[channel] += f64::from(rgb[channel]);
                    let band = pixel / width as usize * 16 / height as usize;
                    band_mean[band][channel] += f64::from(rgb[channel]);
                }
                if value > maximum.0 {
                    maximum = (value, pixel, rgb);
                }
            }
            if maximum.0 > peak {
                peak = maximum.0;
                fs::write(out.join(format!("{label}-peak.rgba32fle")), &bytes)?;
            }
            eprintln!(
                "world_replay {label} frame={index} max={} pixel={} bright={bright:?}",
                maximum.0, maximum.1
            );
            maxima.push(serde_json::json!({"frame":index,"maximum":maximum.0,"pixel":maximum.1,"rgb":maximum.2,"mean":frame_mean.map(|v| v / f64::from(width) / f64::from(height)),"above_1e2_1e4_1e6_1e8":bright}));
        }
        fs::write(out.join(format!("{label}-last.rgba32fle")), &bytes)?;
        let report = serde_json::json!({"mode":label,"extent":extent,"frames":frames,"sample_start":sample_start,"ser":ser != 0,"mean":mean.map(|v| v / f64::from(width) / f64::from(height) / f64::from(frames)),"band_contribution":band_mean.map(|rgb| rgb.map(|v| v / f64::from(width) / f64::from(height) / f64::from(frames))),"peak":peak,"per_frame":maxima});
        fs::write(
            out.join(format!("{label}.json")),
            serde_json::to_vec_pretty(&report)?,
        )?;
        reports.push(report);
    }
    if reports.is_empty() {
        return Err("PRIME_PT_REPLAY_CASE 须为 pt".into());
    }
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&reports)?,
    )?;
    session.finish()?;
    assert_eq!(
        host.errors.load(Ordering::Relaxed),
        0,
        "Vulkan validation错误"
    );
    Ok(())
}
