//! 兼容PRT2测试与迁移输入；生产压缩源接入由Minecraft适配器负责。
use super::{MAX_PACKET, MAX_VERTICES};
use prime_scene::{Material, Mesh, Quad};
use std::collections::HashSet;
const QUAD_BYTES: usize = 84;
#[derive(Clone)]
pub(crate) struct Block<'a> {
    pub(crate) local: [u32; 3],
    pub(crate) offset: [f32; 3],
    pub(crate) quads: &'a [u8],
}

pub(crate) struct Section<'a> {
    pub(crate) id: u64,
    pub(crate) key: [i32; 3],
    pub(crate) available: bool,
    pub(crate) blocks: Vec<Block<'a>>,
    pub(crate) quads: usize,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pub(crate) offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], super::Error> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(super::Error::Input("源包长度溢出"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(super::Error::Input("源包截断"))?;
        self.offset = end;
        Ok(value)
    }

    fn word(&mut self) -> Result<u32, super::Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<Section<'_>>, super::Error> {
    if bytes.len() > MAX_PACKET {
        return Err(super::Error::Input("源包超过 256 MiB"));
    }
    let mut reader = Reader { bytes, offset: 0 };
    let magic = reader.word()?;
    let ticketed = magic == 0x3254_5250;
    if !ticketed && !(cfg!(any(test, feature = "test-fixtures")) && magic == 0x3154_5250) {
        return Err(super::Error::Input("需要带捕获id的PRT2源包"));
    }
    let count = reader.word()? as usize;
    if count == 0 || count > 64 {
        return Err(super::Error::Input("每批需要 1..=64 个 section"));
    }
    let mut sections = Vec::with_capacity(count);
    let mut keys = HashSet::with_capacity(count);
    let mut ids = HashSet::with_capacity(count);
    let mut region = None;
    let mut total_quads = 0usize;
    for _ in 0..count {
        let id = if ticketed {
            let lower = u64::from(reader.word()?);
            let id = lower | (u64::from(reader.word()?) << 32);
            if id == 0 || !ids.insert(id) {
                return Err(super::Error::Input("捕获id为零或重复"));
            }
            id
        } else {
            0
        };
        let key = [
            reader.word()? as i32,
            reader.word()? as i32,
            reader.word()? as i32,
        ];
        if key.iter().any(|v| v.checked_mul(16).is_none()) || !keys.insert(key) {
            return Err(super::Error::Input("section 坐标越界或重复"));
        }
        let cell = key.map(|v| v.div_euclid(4));
        if !ticketed && region.is_some_and(|v| v != cell) {
            return Err(super::Error::Input("section 不属于同一 4³ region"));
        }
        region = Some(cell);
        let blocks = reader.word()? as usize;
        if ticketed && blocks == u32::MAX as usize {
            sections.push(Section {
                id,
                key,
                available: false,
                blocks: Vec::new(),
                quads: 0,
            });
            continue;
        }
        if blocks > 4096 {
            return Err(super::Error::Input("section 超过 4096 个方块"));
        }
        let mut section = Section {
            id,
            key,
            available: true,
            blocks: Vec::with_capacity(blocks),
            quads: 0,
        };
        let mut occupied = [0u64; 64];
        for _ in 0..blocks {
            let local = reader.word()? as usize;
            if local >= 4096 || occupied[local / 64] & (1 << (local % 64)) != 0 {
                return Err(super::Error::Input("方块局部坐标越界或重复"));
            }
            occupied[local / 64] |= 1 << (local % 64);
            let quads = reader.word()? as usize;
            let offset = [reader.word()?, reader.word()?, reader.word()?].map(f32::from_bits);
            if !offset.iter().all(|v| v.is_finite()) {
                return Err(super::Error::Input("模型偏移含非有限值"));
            }
            let data = reader.take(
                quads
                    .checked_mul(QUAD_BYTES)
                    .ok_or(super::Error::Input("quad 数溢出"))?,
            )?;
            for quad in data.as_chunks::<QUAD_BYTES>().0 {
                if quad[4..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|v| !f32::from_le_bytes(*v).is_finite())
                {
                    return Err(super::Error::Input("quad 含非有限坐标或 UV"));
                }
            }
            section.quads += quads;
            section.blocks.push(Block {
                local: [
                    (local & 15) as u32,
                    ((local >> 4) & 15) as u32,
                    (local >> 8) as u32,
                ],
                offset,
                quads: data,
            });
        }
        total_quads += section.quads;
        if total_quads > MAX_VERTICES / 4 {
            return Err(super::Error::Input("批次几何超过顶点预算"));
        }
        sections.push(section);
    }
    if reader.offset != bytes.len() {
        return Err(super::Error::Input("源包含多余字节"));
    }
    Ok(sections)
}

pub(crate) fn compile(section: &Section<'_>) -> Mesh {
    let mut mesh = Mesh {
        quads: Vec::with_capacity(section.quads),
    };
    for block in &section.blocks {
        let offset = std::array::from_fn::<_, 3, _>(|i| block.local[i] as f32 + block.offset[i]);
        for quad in block.quads.as_chunks::<QUAD_BYTES>().0 {
            let tint = u32::from_le_bytes(quad[..4].try_into().unwrap());
            let tint = [16, 8, 0].map(|shift| ((tint >> shift) & 255) as f32 / 255.0);
            let corners = std::array::from_fn::<_, 4, _>(|corner| {
                std::array::from_fn::<_, 5, _>(|i| {
                    let at = 4 + corner * 20 + i * 4;
                    f32::from_le_bytes(quad[at..at + 4].try_into().unwrap())
                })
            });
            mesh.quads.push(Quad::new(
                corners.map(|v| std::array::from_fn(|i| v[i] + offset[i])),
                corners.map(|v| [v[3], v[4]]),
                tint,
                Material::Opaque,
            ));
        }
    }
    mesh
}
