//! 测量适配：保存每128像素区域的原始误差和与16像素区域均值，不要求每个筛选点保存4K图。
//! 计算位于draw计时之外；参考图与比较图均为场景线性RGBA32F，不裁剪负数或亮点。
use std::{fs, path::Path};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const Y: [f64; 3] = [0.2126, 0.7152, 0.0722];

pub(super) struct Reference {
    pixels: Vec<f32>,
    extent: [usize; 2],
    blocks: Vec<[f64; 3]>,
}

fn blocks(pixels: &[f32], extent: [usize; 2]) -> Vec<[f64; 3]> {
    let [w, h] = extent;
    let bw = w.div_ceil(16);
    let mut result = vec![[0.; 3]; bw * h.div_ceil(16)];
    for (i, pixel) in pixels.chunks_exact(4).enumerate() {
        let block = i / w / 16 * bw + i % w / 16;
        for c in 0..3 {
            result[block][c] += f64::from(pixel[c]);
        }
    }
    for (i, v) in result.iter_mut().enumerate() {
        let count = (w - i % bw * 16).min(16) * (h - i / bw * 16).min(16);
        for c in v {
            *c /= count as f64;
        }
    }
    result
}

fn luminance(v: [f64; 3]) -> f64 {
    v.into_iter().zip(Y).map(|(a, b)| a * b).sum()
}

fn block_error(a: &[[f64; 3]], b: &[[f64; 3]], extent: [usize; 2], size: usize) -> [f64; 2] {
    let [w, h] = extent;
    let bw = w.div_ceil(16);
    let step = size / 16;
    let mut result = [0.; 2];
    let mut count = 0;
    for by in 0..h / size {
        for bx in 0..w / size {
            let mut delta = [0.; 3];
            for y in 0..step {
                for x in 0..step {
                    let index = (by * step + y) * bw + bx * step + x;
                    for c in 0..3 {
                        delta[c] += a[index][c] - b[index][c];
                    }
                }
            }
            for v in &mut delta {
                *v /= (step * step) as f64;
            }
            result[0] += delta.iter().map(|v| v * v).sum::<f64>() / 3.;
            result[1] += luminance(delta).powi(2);
            count += 1;
        }
    }
    if count > 0 {
        result.map(|v| v / count as f64)
    } else {
        [0.; 2]
    }
}

impl Reference {
    pub(super) fn load(path: &Path, extent: [u32; 2]) -> Result<Self> {
        let [w, h] = extent.map(|v| v as usize);
        let bytes = fs::read(path)?;
        if bytes.len() != w * h * 16 {
            return Err("参考RGBA32F尺寸不匹配".into());
        }
        let pixels: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().expect("4B float")))
            .collect();
        if pixels.iter().any(|v| !v.is_finite()) {
            return Err("参考图存在非有限值".into());
        }
        let blocks = blocks(&pixels, [w, h]);
        Ok(Self {
            pixels,
            extent: [w, h],
            blocks,
        })
    }

    /// previous是同一配置/预算的首个独立种子；像素数组由测量宿主保活到该配置结束。
    pub(super) fn measure(
        &self,
        image: &[f32],
        previous: Option<&[f32]>,
        dir: &Path,
        label: &str,
    ) -> Result<serde_json::Value> {
        if image.len() != self.pixels.len() || previous.is_some_and(|p| p.len() != image.len()) {
            return Err("测量图尺寸不匹配".into());
        }
        let [w, h] = self.extent;
        let tile_width = w.div_ceil(128);
        let mut tiles = vec![[0f64; 8]; tile_width * h.div_ceil(128)];
        for (i, (pixel, reference)) in image
            .chunks_exact(4)
            .zip(self.pixels.chunks_exact(4))
            .enumerate()
        {
            if pixel.iter().any(|v| !v.is_finite()) || pixel[3] < 0. {
                return Err("待测图无效".into());
            }
            let delta: [f64; 3] =
                std::array::from_fn(|c| f64::from(pixel[c]) - f64::from(reference[c]));
            let tile = &mut tiles[i / w / 128 * tile_width + i % w / 128];
            tile[0] += delta.iter().map(|v| v * v).sum::<f64>() / 3.;
            tile[1] += luminance(delta).powi(2);
            for c in 0..3 {
                tile[c + 2] += delta[c];
            }
            tile[5] += 1.;
            if let Some(previous) = previous {
                let d: [f64; 3] =
                    std::array::from_fn(|c| f64::from(pixel[c]) - f64::from(previous[i * 4 + c]));
                tile[6] += d.iter().map(|v| v * v).sum::<f64>() / 6.;
                tile[7] += luminance(d).powi(2) / 2.;
            }
        }
        let totals: [f64; 8] =
            std::array::from_fn(|c| tiles.iter().map(|v| v[c]).sum::<f64>() / (w * h) as f64);
        let coarse = blocks(image, self.extent);
        let previous_blocks = previous.map(|v| blocks(v, self.extent));
        let mut levels = Vec::new();
        for size in [16, 64, 256] {
            if w < size || h < size {
                continue;
            }
            let error = block_error(&coarse, &self.blocks, self.extent, size);
            let independent = previous_blocks
                .as_ref()
                .map(|other| block_error(&coarse, other, self.extent, size).map(|v| v / 2.));
            levels.push(serde_json::json!({"size":size,"rgb_mse":error[0],"luma_mse":error[1],"independent_variance":independent}));
        }
        let coarse_rgba: Vec<[f32; 4]> = coarse
            .iter()
            .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, 1.])
            .collect();
        let tile_file = format!("{label}-tiles128.f64");
        let block_file = format!("{label}-blocks16.f32");
        fs::write(dir.join(&tile_file), bytemuck::cast_slice(&tiles))?;
        fs::write(dir.join(&block_file), bytemuck::cast_slice(&coarse_rgba))?;
        Ok(
            serde_json::json!({"rgb_mse":totals[0],"luma_mse":totals[1],"bias_rgb":&totals[2..5],
            "independent_rgb_variance":previous.map(|_| totals[6]),"independent_luma_variance":previous.map(|_| totals[7]),
            "blocks":levels,"tile_file":tile_file,"block_file":block_file,
            "tile_extent":[tile_width,h.div_ceil(128)],"tile_fields":["sum_rgb_error2_mean","sum_luma_error2","sum_error_r","sum_error_g","sum_error_b","pixels","sum_independent_rgb_error2_half_mean","sum_independent_luma_error2_half"]}),
        )
    }
}
