//! 资源准备阶段的材质发光功率校准。采样包络和实际 MERS 发光遮罩独立于校准。

use crate::Error;
use simple_engine::light_sampling::emitter::triangle_texel_fractions;

const EXACT_AXIS_LIMIT: u32 = 64;
const HALVES: [[usize; 3]; 2] = [[0, 1, 2], [0, 2, 3]];

/// 两种覆盖各存线性颜色亮度及其与 MERS G 的乘积，供查询实际 UV 下的功率比。
pub(crate) struct PowerMoments {
    extent: [u32; 2],
    texels: Vec<[f64; 4]>,
    total: [f64; 4],
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
        let mut texels = Vec::new();
        texels.try_reserve_exact((extent[0] * extent[1]) as usize)?;
        let mut total = [0.; 4];
        for y in 0..source[1] {
            for x in 0..source[0] {
                let value = texel([x, y]);
                for (sum, value) in total.iter_mut().zip(value) {
                    *sum += value;
                }
                if extent == source {
                    texels.push(value);
                }
            }
        }
        if extent != source {
            texels.push(total);
        }
        Ok(Self {
            extent,
            texels,
            total,
        })
    }

    fn rectangle(&self, lo: [f64; 2], hi: [f64; 2], lane: usize) -> [f64; 2] {
        let lo = std::array::from_fn::<_, 2, _>(|i| lo[i] * f64::from(self.extent[i]));
        let hi = std::array::from_fn::<_, 2, _>(|i| hi[i] * f64::from(self.extent[i]));
        let mut integral = [0.; 2];
        // 仅资源期执行，最多64² texel。只加非负局部项，严格保留空遮罩的零；
        // 四个大前缀相减会在暗区制造极小正功率，除法随后把它放大为极亮光源。
        for y in lo[1].floor() as u32..(hi[1].ceil() as u32).min(self.extent[1]) {
            let dy = hi[1].min(f64::from(y + 1)) - lo[1].max(f64::from(y));
            for x in lo[0].floor() as u32..(hi[0].ceil() as u32).min(self.extent[0]) {
                let dx = hi[0].min(f64::from(x + 1)) - lo[0].max(f64::from(x));
                for (sum, value) in integral.iter_mut().zip(self.texel([x, y], lane)) {
                    *sum += value * dx * dy;
                }
            }
        }
        integral
    }

    fn texel(&self, [x, y]: [u32; 2], lane: usize) -> [f64; 2] {
        let at = (y * self.extent[0] + x) as usize;
        [self.texels[at][lane], self.texels[at][lane + 1]]
    }

    /// 以实际两片世界面积校准局部 UV。过程几何和越界 UV 取整图参考。
    pub(crate) fn ratio(&self, quad: Option<([[f32; 2]; 4], [f64; 2])>, cutout: bool) -> f64 {
        let lane = usize::from(cutout) * 2;
        let whole = self.total;
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

    #[test]
    fn dark_crop_cannot_create_emission_power_from_cancellation() {
        let moments = PowerMoments::new([16, 16], |[x, y]| {
            let mask = if x < 2 { 0.03 * f64::from(y + 1) } else { 0. };
            [1., mask, 1., mask]
        })
        .unwrap();
        // 实际地图异常面的 UV 范围：所有覆盖 texel 的发光遮罩严格为零。
        let uv = [
            [0.375, 0.375],
            [0.375, 0.125],
            [0.125, 0.125],
            [0.125, 0.375],
        ];
        for cutout in [false, true] {
            assert_eq!(moments.ratio(Some((uv, [0.5, 0.5])), cutout), 0.);
        }
    }

    #[test]
    fn nonzero_dim_crop_is_preserved_without_an_epsilon_cutoff() {
        let moments = PowerMoments::new([4, 1], |[x, _]| {
            [1., if x == 1 { 1e-24 } else { 1. }, 0., 0.]
        })
        .unwrap();
        let uv = [[0.25, 0.], [0.5, 0.], [0.5, 1.], [0.25, 1.]];
        let ratio = moments.ratio(Some((uv, [0.5, 0.5])), false);
        assert!((ratio / 1e24 - 1.).abs() < 1e-15);
    }

    #[test]
    fn dark_triangle_and_constant_uv_keep_zero_power() {
        let moments = PowerMoments::new([16, 16], |[x, y]| {
            [1., if x < 2 { 0.03 * f64::from(y + 1) } else { 0. }, 0., 0.]
        })
        .unwrap();
        for uv in [
            [[0.125, 0.125], [0.375, 0.125], [0.25, 0.375], [0.25, 0.375]],
            [[0.25, 0.25]; 4],
        ] {
            assert_eq!(moments.ratio(Some((uv, [0.5, 0.])), false), 0.);
        }
    }
}
