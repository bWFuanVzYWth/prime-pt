//! 冻结原版PBR sheet的真实帧布局；与Java albedo分辨率及帧序列分别解释。
//!
//! 普通PNG未命中动画表时是整张单帧，包括32²材质映射16²albedo的情况。
//! 归一化按目标像素中心覆盖完整逻辑帧，不把Java tile尺寸当作材质PNG帧尺寸。
use crate::Error;

struct Frozen {
    name: &'static str,
    sheet: [u32; 2],
    frame: [u32; 2],
    sequence: &'static [[u32; 2]],
    interpolate: bool,
}
include!("../assets/vanilla_pbr/frames.rs");

/// 一张已校验sheet的只读布局；Copy只复制尺寸及静态序列引用，不复制像素。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout<'a> {
    sheet: [u32; 2],
    frame: [u32; 2],
    sequence: &'a [[u32; 2]],
    interpolate: bool,
}
impl<'a> Layout<'a> {
    pub(crate) const fn from_validated(
        sheet: [u32; 2],
        frame: [u32; 2],
        sequence: &'a [[u32; 2]],
        interpolate: bool,
    ) -> Self {
        Self {
            sheet,
            frame,
            sequence,
            interpolate,
        }
    }
    /// 材质PNG内单个逻辑帧尺寸，独立于Java sprite tile。
    pub const fn frame_extent(self) -> [u32; 2] {
        self.frame
    }
    /// 完整材质时间序列`[sheet_frame_index,duration_ticks]`；空为静态frame0。
    /// 资源发光包络须访问此序列，即使Java albedo没有动画；重复帧可由调用者去重。
    pub const fn frames(self) -> &'a [[u32; 2]] {
        self.sequence
    }
    /// 原始时间序列的插值标志；保存它不代表已经接入运行期动画播放。
    pub const fn interpolate(self) -> bool {
        self.interpolate
    }
    /// sheet可容纳的物理帧数量，不是时间序列条数。
    pub const fn frame_count(self) -> u32 {
        (self.sheet[0] / self.frame[0]) * (self.sheet[1] / self.frame[1])
    }
    /// 将目标tile像素中心归一化到指定材质物理帧，返回PNG sheet整数坐标。
    ///
    /// frame是sheet frame index而非时间序列位置；超出物理帧数沿旧材质规则钳制最后帧。
    /// 初次显示可取frames首项的index；静态序列取0。最近邻中心规则32→16读取1,3,..31。
    ///
    /// # Panics
    /// target必须非零且texel逐轴小于target。调用者应在资源边界校验目标tile，再按其范围遍历。
    pub const fn source_texel(self, frame: u32, texel: [u32; 2], target: [u32; 2]) -> [u32; 2] {
        assert!(
            target[0] > 0 && target[1] > 0 && texel[0] < target[0] && texel[1] < target[1],
            "目标tile像素越界"
        );
        let count = self.frame_count();
        let frame = if frame < count { frame } else { count - 1 };
        let columns = self.sheet[0] / self.frame[0];
        let x = ((texel[0] as u64 * 2 + 1) * self.frame[0] as u64 / (target[0] as u64 * 2)) as u32;
        let y = ((texel[1] as u64 * 2 + 1) * self.frame[1] as u64 / (target[1] as u64 * 2)) as u32;
        [
            frame % columns * self.frame[0] + x,
            frame / columns * self.frame[1] + y,
        ]
    }
}

/// 按明确原版sprite身份解析26.3帧布局，并校验解码PNG尺寸。
/// 未命中冻结动画表时使用整PNG静态帧；零或超过16384的尺寸、已知sheet尺寸不符返回错误。
/// 这里没有I/O、像素分配、JSON解析或缓存；资源所有者只需每sheet建立一次。
pub fn layout(name: &str, sheet_extent: [u32; 2]) -> Result<Layout<'static>, Error> {
    if sheet_extent.iter().any(|&v| v == 0 || v > 16384) {
        return Err(Error::Input("材质sheet尺寸无效"));
    }
    if let Ok(index) = FROZEN.binary_search_by_key(&name, |v| v.name) {
        let entry = &FROZEN[index];
        if entry.sheet != sheet_extent {
            return Err(Error::Input("材质sheet与冻结帧布局不一致"));
        }
        Ok(Layout {
            sheet: entry.sheet,
            frame: entry.frame,
            sequence: entry.sequence,
            interpolate: entry.interpolate,
        })
    } else {
        Ok(Layout {
            sheet: sheet_extent,
            frame: sheet_extent,
            sequence: &[],
            interpolate: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn static_high_resolution_material_covers_the_whole_frame() {
        let l = layout("minecraft:block/acacia_shelf", [32, 32]).unwrap();
        assert!(l.frames().is_empty());
        assert_eq!(l.frame_count(), 1);
        let xs: Vec<_> = (0..16)
            .map(|x| l.source_texel(0, [x, 0], [16, 16])[0])
            .collect();
        assert_eq!(xs, (0..16).map(|x| x * 2 + 1).collect::<Vec<_>>());
        assert_eq!(l.source_texel(9, [15, 15], [16, 16]), [31, 31]);
    }
    #[test]
    fn animated_sheet_uses_material_timeline_and_frame_coordinates() {
        let l = layout("minecraft:block/blast_furnace_front_on", [16, 32]).unwrap();
        assert_eq!(l.frames(), [[0, 10], [1, 10]]);
        assert!(l.interpolate());
        assert_eq!(l.frame_extent(), [16, 16]);
        assert_eq!(l.source_texel(0, [15, 15], [16, 16]), [15, 15]);
        assert_eq!(l.source_texel(1, [15, 15], [16, 16]), [15, 31]);
        assert!(layout("minecraft:block/blast_furnace_front_on", [16, 16]).is_err());
    }
    #[test]
    fn all_frozen_sequences_stay_inside_the_declared_sheet() {
        assert!(FROZEN.windows(2).all(|v| v[0].name < v[1].name));
        for e in &FROZEN {
            let l = layout(e.name, e.sheet).unwrap();
            assert!(
                e.sheet
                    .into_iter()
                    .zip(e.frame)
                    .all(|(s, f)| f > 0 && s % f == 0)
            );
            assert!(!l.frames().is_empty());
            for &[frame, duration] in l.frames() {
                assert!(duration > 0 && frame < l.frame_count());
                let [x, y] = l.source_texel(frame, [0, 0], [1, 1]);
                assert!(x < e.sheet[0] && y < e.sheet[1]);
            }
        }
    }
}
