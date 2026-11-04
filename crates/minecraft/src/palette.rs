//! PRT3: magic/count/generation，随后ticket/key/status/bits/palette-count/word-count及数组。
//! 压缩存储采用Minecraft的整word分组，无跨word字段，index=(y*16+z)*16+x。

use crate::{Error, wire::Reader};
use std::collections::HashSet;

/// 已校验的16³方块压缩存储；资源ID解释由同一世代Catalog负责。
#[derive(Debug, PartialEq, Eq)]
pub struct SectionStorage {
    bits: u32,
    palette: Vec<u32>,
    words: Vec<u64>,
}

impl SectionStorage {
    pub(crate) fn palette_values(&self) -> &[u32] {
        &self.palette
    }
    /// 接收独占palette/storage，不复制4096个展开状态。
    /// bits=0须恰好一个palette项且无word；空palette表示全局registry ID直接编码。
    pub fn new(bits: u32, palette: Vec<u32>, words: Vec<u64>) -> Result<Self, Error> {
        if bits > 32 || palette.len() > 4096 {
            return Err(Error::Input("section palette布局无效"));
        }
        if bits == 0 {
            if palette.len() != 1 || !words.is_empty() {
                return Err(Error::Input("单状态section须一个palette项且无storage"));
            }
        } else {
            let per_word = 64 / bits as usize;
            if words.len() != 4096usize.div_ceil(per_word)
                || !palette.is_empty() && palette.len() as u64 > 1u64 << bits
            {
                return Err(Error::Input("section storage长度或palette容量无效"));
            }
            if !palette.is_empty() {
                let mask = (1u64 << bits) - 1;
                for index in 0..4096 {
                    let value =
                        (words[index / per_word] >> (index % per_word * bits as usize)) & mask;
                    if value >= palette.len() as u64 {
                        return Err(Error::Input("section palette索引越界"));
                    }
                }
            }
        }
        Ok(Self {
            bits,
            palette,
            words,
        })
    }

    /// 返回一个已验证域内的状态ID；index为(y*16+z)*16+x，超出4096时返回None。
    pub fn state(&self, index: usize) -> Option<u32> {
        if index >= 4096 {
            return None;
        }
        let value = if self.bits == 0 {
            0
        } else {
            let per_word = 64 / self.bits as usize;
            ((self.words[index / per_word] >> (index % per_word * self.bits as usize))
                & ((1u64 << self.bits) - 1)) as usize
        };
        Some(if self.palette.is_empty() {
            value as u32
        } else {
            self.palette[value]
        })
    }

    /// 比较同一资源世代的状态ID，返回实际变化的最小半开包围盒，坐标顺序为[x, y, z]。
    /// palette顺序、重复ID或未使用的填充位不构成状态变化；完全等价时返回None。
    pub fn changed_bounds(&self, next: &Self) -> Option<([u8; 3], [u8; 3])> {
        const FULL: ([u8; 3], [u8; 3]) = ([0; 3], [16; 3]);
        match (self.uniform_state(), next.uniform_state()) {
            (Some(old), Some(new)) => return (old != new).then_some(FULL),
            (Some(id), None) if !next.palette.is_empty() && !next.palette.contains(&id) => {
                return Some(FULL);
            }
            (None, Some(id)) if !self.palette.is_empty() && !self.palette.contains(&id) => {
                return Some(FULL);
            }
            _ => {}
        }
        let mut min = [16; 3];
        let mut max_exclusive = [0; 3];
        let mut include = |index: usize| {
            let position = [
                (index & 15) as u8,
                (index >> 8) as u8,
                ((index >> 4) & 15) as u8,
            ];
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max_exclusive[axis] = max_exclusive[axis].max(position[axis] + 1);
            }
        };
        let same_index_layout = self.bits == next.bits
            && self.palette.is_empty() == next.palette.is_empty()
            && (self.palette.starts_with(&next.palette) || next.palette.starts_with(&self.palette));
        if same_index_layout {
            if self.words == next.words {
                return None;
            }
            let bits = self.bits as usize;
            let per_word = 64 / bits;
            let mask = (1u64 << bits) - 1;
            for (word, (&old, &new)) in self.words.iter().zip(&next.words).enumerate() {
                let start = word * per_word;
                let mut changed = old ^ new;
                while changed != 0 {
                    let field = changed.trailing_zeros() as usize / bits;
                    let index = start + field;
                    if field >= per_word || index >= 4096 {
                        break;
                    }
                    let shift = field * bits;
                    if self.palette.is_empty()
                        || self.palette[((old >> shift) & mask) as usize]
                            != next.palette[((new >> shift) & mask) as usize]
                    {
                        include(index);
                    }
                    changed &= !(mask << shift);
                }
            }
        } else {
            for index in 0..4096 {
                if self.state(index) != next.state(index) {
                    include(index);
                }
            }
        }
        (min[0] < 16).then_some((min, max_exclusive))
    }

    /// 实际拥有的有效源字节，不含容器元数据或保留容量。
    pub fn source_bytes(&self) -> usize {
        self.palette.len() * 4 + self.words.len() * 8
    }

    /// 单状态压缩源的真实registry ID；多状态布局返回None，不猜测ID0为air。
    pub fn uniform_state(&self) -> Option<u32> {
        (self.bits == 0).then(|| self.palette[0])
    }
}

/// 一条自持捕获响应；Unavailable与已加载的空section明确分离。
#[derive(Debug)]
pub struct SectionSource {
    /// 原24B请求票据；零值无效，同包不得重复。
    pub request: u64,
    /// 原16m section坐标，不是64m region坐标。
    pub key: [i32; 3],
    /// 与资源Catalog相同的非零资源世代。
    pub generation: u64,
    /// None表示宿主明确Unavailable；Some可包含单一air状态。
    pub storage: Option<SectionStorage>,
}

/// 解码PRT3压缩响应，拥有输入数组；调用结束后Java可复用源ByteBuffer。
/// 全包布局校验后才返回，不接受重复ticket、尾部或无效压缩索引。
/// 状态身份由接受方使用[`Catalog::validate_section`](crate::Catalog::validate_section)校验。
pub fn decode_sections(bytes: &[u8]) -> Result<Vec<SectionSource>, Error> {
    let mut reader = Reader::new(bytes)?;
    if reader.u32()? != 0x3354_5250 {
        return Err(Error::Input("需要PRT3 palette/storage源包"));
    }
    let count = reader.count(36)?;
    let generation = reader.u64()?;
    if generation == 0 || count > 64 {
        return Err(Error::Input("PRT3资源世代或响应数量无效"));
    }
    let mut seen = HashSet::new();
    seen.try_reserve(count)?;
    let mut output = Vec::new();
    output.try_reserve_exact(count)?;
    for _ in 0..count {
        let request = reader.u64()?;
        let key = [reader.i32()?, reader.i32()?, reader.i32()?];
        let status = reader.u32()?;
        let bits = reader.u32()?;
        let palette_count = reader.u32()? as usize;
        let words_count = reader.u32()? as usize;
        if request == 0
            || !seen.insert(request)
            || status > 1
            || palette_count > 4096
            || words_count > 4096
        {
            return Err(Error::Input("PRT3票据或section布局无效"));
        }
        let storage = if status == 0 {
            if bits != 0 || palette_count != 0 || words_count != 0 {
                return Err(Error::Input("Unavailable不能携带压缩状态"));
            }
            None
        } else {
            let mut palette = Vec::new();
            palette.try_reserve_exact(palette_count)?;
            for _ in 0..palette_count {
                palette.push(reader.u32()?);
            }
            let mut words = Vec::new();
            words.try_reserve_exact(words_count)?;
            for _ in 0..words_count {
                words.push(reader.u64()?);
            }
            Some(SectionStorage::new(bits, palette, words)?)
        };
        output.push(SectionSource {
            request,
            key,
            generation,
            storage,
        });
    }
    reader.finish()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_fields_stop_at_word_boundaries_and_resolve_palette_ids() {
        let mut words = vec![0; 4096usize.div_ceil(12)];
        for index in 0..4096 {
            words[index / 12] |= ((index % 19) as u64) << (index % 12 * 5);
        }
        let palette: Vec<_> = (100..119).collect();
        let storage = SectionStorage::new(5, palette, words).unwrap();
        assert!((0..4096).all(|i| storage.state(i) == Some(100 + (i % 19) as u32)));
        assert_eq!(storage.state(4096), None);
    }

    #[test]
    fn changed_bounds_ignore_packed_padding_and_equivalent_palette_slots() {
        let old_words = vec![0; 4096usize.div_ceil(12)];
        let mut new_words = old_words.clone();
        new_words[0] = (1 << 55) | (15 << 60);
        new_words[1] = 1 | (2 << 20);
        *new_words.last_mut().unwrap() = u64::MAX << 20;
        let old = SectionStorage::new(5, vec![71, 91, 71], old_words).unwrap();
        let new = SectionStorage::new(5, vec![71, 91, 71], new_words).unwrap();
        assert_eq!(old.changed_bounds(&new), Some(([11, 0, 0], [13, 1, 1])));
    }

    #[test]
    fn changed_bounds_compare_ids_across_palette_layouts() {
        let old = SectionStorage::new(1, vec![71, 91], vec![1; 64]).unwrap();
        let mut new_words = vec![0x5555_5555_5555_5555; 128];
        for word in new_words.iter_mut().step_by(2) {
            *word &= !1;
        }
        let equivalent = SectionStorage::new(2, vec![91, 71], new_words.clone()).unwrap();
        assert_eq!(old.changed_bounds(&equivalent), None);
        new_words[0] &= !(1 << 2);
        let changed = SectionStorage::new(2, vec![91, 71], new_words).unwrap();
        assert_eq!(old.changed_bounds(&changed), Some(([1, 0, 0], [2, 1, 1])));
    }

    #[test]
    fn changed_bounds_accept_new_entries_in_same_width_palette() {
        let old_words = vec![0; 256];
        let mut new_words = old_words.clone();
        new_words[255] = 2 << 60;
        let old = SectionStorage::new(4, vec![71, 91], old_words).unwrap();
        let new = SectionStorage::new(4, vec![71, 91, 101], new_words).unwrap();
        assert_eq!(old.changed_bounds(&new), Some(([15; 3], [16; 3])));
    }

    #[test]
    fn single_and_global_palettes_keep_their_actual_source_meaning() {
        assert_eq!(
            SectionStorage::new(0, vec![71], vec![])
                .unwrap()
                .state(4095),
            Some(71)
        );
        assert_eq!(
            SectionStorage::new(32, vec![], vec![0xdead_beef_1234_5678; 2048])
                .unwrap()
                .state(1),
            Some(0xdead_beef)
        );
        assert!(SectionStorage::new(0, vec![], vec![]).is_err());
        assert!(SectionStorage::new(4, vec![0, 1], vec![u64::MAX; 256]).is_err());
    }
}
