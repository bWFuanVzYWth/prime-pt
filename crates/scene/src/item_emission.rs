//! 物品实例的表面自发光。方块状态决定无贴图时的强度，sprite 决定逐纹素遮罩。
use std::collections::HashMap;

/// 一张发光 sprite 在世代发光数据中的地址及各亮度等级的功率校准。
pub struct ItemEmissionMask {
    /// 非零 word 偏移。头部为四个 atlas 边界、打包的 16 位宽高，零尺寸引用地形 MERS，其余随后为每纹素一字节。
    pub word: u32,
    /// 第一组适用于完整面，第二组适用于 alpha 裁切面。每组按原版亮度 0–15 索引。
    pub gains: [[f32; 16]; 2],
}

/// 与 GPU 发光数据同次发布的物品材质索引。实体身份、环境亮度均不参与选择。
#[derive(Default)]
pub struct ItemMaterials {
    /// 方块状态 ID 对应本征亮度与方块是否使用自定义发光遮罩。
    pub states: HashMap<u32, (u8, bool)>,
    /// 只保存实际含有自发光纹素的 sprite。
    pub masks: HashMap<u32, ItemEmissionMask>,
}

impl ItemMaterials {
    /// 原版亮度变为场景线性发光系数。输入是 0–15 的方块或模型面亮度。
    pub const fn native_amplitude(level: u32) -> f32 {
        let x = level as f32 / 15.;
        x * x
    }

    /// 返回遮罩 word 偏移和线性强度。零偏移表示均匀表面，未知方块状态返回 None。
    /// 非方块物品用 None，可由模型面亮度或资源包贴图独立发光。
    pub fn surface(
        &self,
        state: Option<u32>,
        sprite: u32,
        face: u32,
        cutout: bool,
    ) -> Option<(u32, f32)> {
        let (base, masked) = match state {
            Some(id) => *self.states.get(&id)?,
            None => (0, false),
        };
        let level = face.max(u32::from(base));
        if level > 15 {
            return None;
        }
        Some(if let Some(mask) = self.masks.get(&sprite) {
            (mask.word, mask.gains[usize::from(cutout)][level as usize])
        } else {
            (
                0,
                if masked {
                    0.
                } else {
                    Self::native_amplitude(level)
                },
            )
        })
    }

    /// 校验每个可见遮罩头、纹素数据与查找表使用同一已上传缓冲。
    pub fn valid_for(&self, words: &[u32]) -> bool {
        self.states.values().all(|&(level, _)| level <= 15)
            && self.masks.values().all(|mask| {
                let start = mask.word as usize;
                let Some(header) = words.get(start..).and_then(|tail| tail.first_chunk::<5>())
                else {
                    return false;
                };
                let bounds = [header[0], header[1], header[2], header[3]].map(f32::from_bits);
                let w = header[4] & 65535;
                let h = header[4] >> 16;
                start != 0
                    && bounds.iter().all(|v| (0. ..=1.).contains(v))
                    && bounds[0] < bounds[2]
                    && bounds[1] < bounds[3]
                    && (w == 0) == (h == 0)
                    && (w * h).div_ceil(4) as usize <= words.len() - start - 5
                    && mask
                        .gains
                        .iter()
                        .flatten()
                        .all(|v| v.is_finite() && *v >= 0.)
            })
    }
}
