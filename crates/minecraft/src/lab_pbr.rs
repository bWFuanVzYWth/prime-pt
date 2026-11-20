//! shaderLABS LabPBR 1.3 伴随纹理与宿主动画布局。
//! 通道按原始数据读取，避免颜色管理改变法线、材质编号和发光哨兵。
//! [材质标准](https://shaderlabs.org/wiki/LabPBR_Material_Standard)
use crate::{Error, ImageSource, MAX_PACKET, vanilla_frames::Layout, wire::Reader};
use std::io::Cursor;

/// 一个材质资源拥有原始 RGBA8 像素、已验证的逻辑帧尺寸和帧序列。
pub struct MaterialImage {
    /// 解码后的完整图集，按 PNG 行序排列。
    pub(crate) image: ImageSource,
    frame: [u32; 2],
    frames: Vec<[u32; 2]>,
    interpolate: bool,
}
impl MaterialImage {
    pub(crate) fn inherited(
        image: ImageSource,
        base: &ImageSource,
        tile: [u32; 2],
        frames: &[[u32; 2]],
        interpolate: bool,
    ) -> Result<Self, Error> {
        let grid = [base.width / tile[0], base.height / tile[1]];
        let own = [image.width, image.height];
        let parallel = !frames.is_empty()
            && own[0].is_multiple_of(grid[0])
            && own[1].is_multiple_of(grid[1])
            && u64::from(own[0]) * u64::from(base.height)
                == u64::from(own[1]) * u64::from(base.width);
        let mut sequence = Vec::new();
        if parallel {
            sequence.try_reserve_exact(frames.len())?;
            sequence.extend_from_slice(frames);
        }
        Ok(Self {
            image,
            frame: if parallel {
                [own[0] / grid[0], own[1] / grid[1]]
            } else {
                own
            },
            frames: sequence,
            interpolate: parallel && interpolate,
        })
    }
    pub(crate) fn read(
        r: &mut Reader<'_>,
        base: &ImageSource,
        tile: [u32; 2],
        frames: &[[u32; 2]],
        base_interpolate: bool,
    ) -> Result<Self, Error> {
        let len = r.count(1)?;
        let image = decode_png(r.take(len)?)?;
        match r.u32()? {
            0 => return Self::inherited(image, base, tile, frames, base_interpolate),
            1 => (),
            _ => return Err(Error::Input("材质动画标记无效")),
        }
        let [w, h] = [r.u32()?, r.u32()?];
        let minimum = image.width.min(image.height);
        let frame = match (w, h) {
            (0, 0) => [minimum; 2],
            (0, h) => [image.width, h],
            (w, 0) => [w, image.height],
            _ => [w, h],
        };
        let default_time = r.u32()?;
        let interpolate = r.u32()?;
        if default_time == 0
            || interpolate > 1
            || !image.width.is_multiple_of(frame[0])
            || !image.height.is_multiple_of(frame[1])
        {
            return Err(Error::Input("LabPBR 动画帧尺寸或时长无效"));
        }
        let count = r.count(8)?;
        let capacity = (image.width / frame[0]) * (image.height / frame[1]);
        let mut sequence = Vec::new();
        if count == 0 {
            sequence.try_reserve_exact(capacity as usize)?;
            sequence.extend((0..capacity).map(|i| [i, default_time]));
        } else {
            sequence.try_reserve_exact(count)?;
            for _ in 0..count {
                let next = [r.u32()?, r.u32()?];
                if next[0] >= capacity || next[1] == 0 {
                    return Err(Error::Input("LabPBR 动画序列越界"));
                }
                sequence.push(next);
            }
        }
        Ok(Self {
            image,
            frame,
            frames: sequence,
            interpolate: interpolate != 0,
        })
    }
    /// 返回已验证的只读帧布局。目标 atlas 通过像素中心采样整个逻辑帧。
    pub fn layout(&self) -> Layout<'_> {
        Layout::from_validated(
            [self.image.width, self.image.height],
            self.frame,
            &self.frames,
            self.interpolate,
        )
    }
}

fn decode_png(bytes: &[u8]) -> Result<ImageSource, Error> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let (width, height) = reader.info().size();
    let size = u64::from(width) * u64::from(height) * 4;
    if width > 16384 || height > 16384 || size > MAX_PACKET as u64 {
        return Err(Error::Input("材质 PNG 超过源图像上限"));
    }
    let len = reader
        .output_buffer_size()
        .ok_or(Error::Input("材质 PNG 输出越界"))?;
    let mut src = Vec::new();
    src.try_reserve_exact(len)?;
    src.resize(len, 0);
    let info = reader.next_frame(&mut src)?;
    if info.width != width || info.height != height {
        return Err(Error::Input("材质 PNG 首帧必须覆盖整张图像"));
    }
    let channels = info.color_type.samples();
    let pixels = if channels == 4 {
        src
    } else {
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(size as usize)?;
        for p in src[..info.buffer_size()].chunks_exact(channels) {
            rgba.extend_from_slice(&match p {
                &[g] => [g, g, g, 255],
                &[g, a] => [g, g, g, a],
                &[r, g, b] => [r, g, b, 255],
                _ => return Err(Error::Input("无法识别材质 PNG 的通道布局")),
            });
        }
        rgba
    };
    Ok(ImageSource {
        width,
        height,
        pixels,
    })
}

pub(crate) const fn emission(a: u8) -> u8 {
    if a == 255 {
        0
    } else {
        ((a as u16 * 255 + 127) / 254) as u8
    }
}
pub(crate) const fn mers([smooth, f0, porosity_sss, a]: [u8; 4]) -> [u8; 4] {
    [
        if f0 >= 230 { 255 } else { 0 },
        emission(a),
        255 - smooth,
        ((porosity_sss.saturating_sub(65) as u16 * 255 + 95) / 190) as u8,
    ]
}
