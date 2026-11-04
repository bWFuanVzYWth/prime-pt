//! 资源准备阶段的材质发光功率校准。采样包络和实际 MERS 发光遮罩独立于校准。

use crate::Error;
use light_sampling::emitter::triangle_texel_fractions;

const EXACT_AXIS_LIMIT: u32 = 64;
const HALVES: [[usize; 3]; 2] = [[0, 1, 2], [0, 2, 3]];

/// 两种覆盖各存线性颜色亮度及其与 MERS G 的乘积，供查询实际 UV 下的功率比。
pub(crate) struct PowerMoments {
    extent: [u32; 2],
    prefix: Vec<[f64; 4]>,
}

impl PowerMoments {
    /// 源尺寸非零。任一轴超过上限时只保留整图积分，限制准备工作集。
    pub(crate) fn new(
        source: [u32; 2],
        mut texel: impl FnMut([u32; 2]) -> [f64; 4],
    ) -> Result<Self, Error> {
        let extent = if source.into_iter().all(|v| v <= EXACT_AXIS_LIMIT) {
            source
        } else {
            [1, 1]
        };
        let stride = extent[0] as usize + 1;
        let count = stride * (extent[1] as usize + 1);
        let mut prefix = Vec::new();
        prefix.try_reserve_exact(count)?;
        prefix.resize(count, [0.; 4]);
        if extent != source {
            for y in 0..source[1] {
                for x in 0..source[0] {
                    for (sum, value) in prefix[3].iter_mut().zip(texel([x, y])) {
                        *sum += value;
                    }
                }
            }
        } else {
            for y in 0..extent[1] {
                let mut row = [0.; 4];
                for x in 0..extent[0] {
                    let above = y as usize * stride + x as usize + 1;
                    for (sum, value) in row.iter_mut().zip(texel([x, y])) {
                        *sum += value;
                    }
                    prefix[above + stride] = std::array::from_fn(|i| prefix[above][i] + row[i]);
                }
            }
        }
        Ok(Self { extent, prefix })
    }

    fn cumulative(&self, uv: [f64; 2], lane: usize) -> [f64; 2] {
        let [w, h] = self.extent.map(|v| v as usize);
        let p = [uv[0] * w as f64, uv[1] * h as f64];
        let [x, y] = p.map(|v| v.floor() as usize);
        let [fx, fy] = [p[0] - x as f64, p[1] - y as f64];
        let at = |dx: usize, dy: usize, i: usize| {
            self.prefix[(y + dy).min(h) * (w + 1) + (x + dx).min(w)][lane + i]
        };
        std::array::from_fn(|i| {
            let a = at(0, 0, i) * (1. - fx) + at(1, 0, i) * fx;
            let b = at(0, 1, i) * (1. - fx) + at(1, 1, i) * fx;
            a * (1. - fy) + b * fy
        })
    }

    fn rectangle(&self, lo: [f64; 2], hi: [f64; 2], lane: usize) -> [f64; 2] {
        let a = self.cumulative(lo, lane);
        let b = self.cumulative([hi[0], lo[1]], lane);
        let c = self.cumulative([lo[0], hi[1]], lane);
        let d = self.cumulative(hi, lane);
        std::array::from_fn(|i| d[i] - b[i] - c[i] + a[i])
    }

    fn texel(&self, [x, y]: [u32; 2], lane: usize) -> [f64; 2] {
        let stride = self.extent[0] as usize + 1;
        let at = y as usize * stride + x as usize;
        std::array::from_fn(|i| {
            let i = lane + i;
            self.prefix[at + stride + 1][i] - self.prefix[at + stride][i] - self.prefix[at + 1][i]
                + self.prefix[at][i]
        })
    }

    /// 以实际两片世界面积校准局部 UV。过程几何和越界 UV 取整图参考。
    pub(crate) fn ratio(&self, quad: Option<([[f32; 2]; 4], [f64; 2])>, cutout: bool) -> f64 {
        let lane = usize::from(cutout) * 2;
        let whole = self.prefix[self.prefix.len() - 1];
        let whole = [whole[lane], whole[lane + 1]];
        let [color, textured] = match quad {
            Some((uv, areas))
                if self.extent != [1, 1]
                    && uv.iter().flatten().all(|v| (0.0..=1.0).contains(v)) =>
            {
                self.integral(uv, areas, whole, lane)
            }
            _ => whole,
        };
        if textured > 0. { color / textured } else { 0. }
    }

    fn integral(
        &self,
        uv: [[f32; 2]; 4],
        areas: [f64; 2],
        whole: [f64; 2],
        lane: usize,
    ) -> [f64; 2] {
        let coordinates = uv.map(|p| p.map(f64::from));
        let triangles = HALVES.map(|half| half.map(|i| coordinates[i]));
        let doubled_uv_areas = triangles
            .map(|[a, b, c]| ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs());
        let [left, right] = [
            areas[0] * doubled_uv_areas[1],
            areas[1] * doubled_uv_areas[0],
        ];
        let rectangle = (0..4).all(|i| {
            let a = coordinates[i];
            let b = coordinates[(i + 1) % 4];
            (a[0] == b[0]) != (a[1] == b[1])
        });
        // 轴对齐矩形的两半共享面积 Jacobian 时，公共因子在功率比中抵消。
        if rectangle
            && doubled_uv_areas.into_iter().all(|v| v > 0.)
            && (left - right).abs() <= 1e-9 * left.max(right)
        {
            let lo = std::array::from_fn(|i| coordinates.iter().map(|p| p[i]).fold(1., f64::min));
            let hi = std::array::from_fn(|i| coordinates.iter().map(|p| p[i]).fold(0., f64::max));
            return self.rectangle(lo, hi, lane);
        }
        let mut integral = [0.; 2];
        for (half, indices) in HALVES.into_iter().enumerate() {
            let area = areas[half];
            if area == 0. {
                continue;
            }
            let mut add = |texel: [f64; 2], fraction: f64| {
                for (sum, value) in integral.iter_mut().zip(texel) {
                    *sum += value * fraction * area;
                }
            };
            if doubled_uv_areas[half] > 0. {
                triangle_texel_fractions(indices.map(|i| uv[i]), self.extent, |texel, fraction| {
                    add(self.texel(texel, lane), fraction);
                })
                .expect("非退化 UV 与精确网格已经校验");
            } else if triangles[half].iter().all(|p| *p == triangles[half][0]) {
                let cell = std::array::from_fn(|i| {
                    let size = self.extent[i];
                    ((triangles[half][0][i] * f64::from(size)) as u32).min(size - 1)
                });
                add(self.texel(cell, lane), 1.);
            } else {
                // 秩一 UV 没有面积映射，沿用整张纹理参考功率。
                add(whole, 1. / f64::from(self.extent[0] * self.extent[1]));
            }
        }
        integral
    }
}

/// 将旧版 50% 功率混合折叠进 MERS 振幅，保留逐 texel 的 G 遮罩。
pub(crate) fn textured_amplitude(native: f32, ratio: f64) -> f32 {
    let gain = (f64::from(native) * ratio) as f32;
    if gain > 0. && gain.is_finite() {
        0.5 + 0.5 * gain
    } else {
        1.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_texels() -> PowerMoments {
        PowerMoments::new([2, 1], |[x, _]| [1., f64::from(x) * 0.25, 0., 0.]).unwrap()
    }

    #[test]
    fn rectangular_crop_integrates_fractional_texels() {
        let uv = [[0.125, 0.], [0.625, 0.], [0.625, 1.], [0.125, 1.]];
        assert_eq!(two_texels().ratio(Some((uv, [0.5, 0.5])), false), 16.);
    }

    #[test]
    fn triangular_crop_weights_texels_by_covered_world_area() {
        let uv = [[0., 0.], [1., 0.], [1., 1.], [1., 1.]];
        assert_eq!(two_texels().ratio(Some((uv, [1., 0.])), false), 16. / 3.);
    }
}
