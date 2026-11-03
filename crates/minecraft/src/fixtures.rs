//! 固定原始资源/压缩源fixture；消费生产decode、材质准备及编译入口，不代表Java宿主成本。
use crate::{Catalog, Error};

fn word(bytes: &mut Vec<u8>, v: u32) {
    bytes.extend(v.to_le_bytes());
}
fn long(bytes: &mut Vec<u8>, v: u64) {
    bytes.extend(v.to_le_bytes());
}
fn text(bytes: &mut Vec<u8>, v: &str) {
    word(bytes, v.len() as u32);
    bytes.extend(v.as_bytes());
}
fn floats(bytes: &mut Vec<u8>, v: impl IntoIterator<Item = f32>) {
    for v in v {
        word(bytes, v.to_bits());
    }
}

/// 原始palette基准输入，几何与tint都由生产Catalog编译。
#[derive(Clone, Copy)]
pub enum Pattern {
    /// 单一air状态，验证真实字典驱动的空源快速路径。
    Air,
    /// 4096个相邻cube，主要消费剔面与可见模型展开。
    Dense,
    /// 每四格一个独立cube，消费稀疏可见模型。
    Sparse,
    /// 原始水状态，消费流体3³邻域、斜顶和UV。
    Fluid,
    /// 4096个真实tint槽模型，输出显式宿主求值请求。
    Tinted,
}

/// 完整PRM1原始字典，包含air、两个实际六面模型、流体和两个sprite。
pub fn resource_packet() -> Vec<u8> {
    resource_packet_with_fluid_fields(0, false)
}

pub(crate) fn resource_packet_with_fluid_fields(support: u32, falling: bool) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [0x314d5250, 263] {
        word(&mut b, v);
    }
    long(&mut b, 1);
    for v in [1, 4, 2, 0, 1, 2] {
        word(&mut b, v);
    }
    for id in 0..4u32 {
        let flags = match id {
            0 => 1,
            3 => 16,
            _ => 4,
        };
        let model = match id {
            1 => 1,
            2 => 2,
            _ => 0,
        };
        for v in [id, flags, model] {
            word(&mut b, v);
        }
        text(&mut b, &format!("fixture:state_{id}"));
        for _ in 0..6 {
            word(&mut b, if id == 1 || id == 2 { 1 } else { 0 });
        }
        for v in [
            support,
            if id == 3 { 1 } else { 0 },
            if id == 3 { 1 } else { 0 },
            0,
            u32::from(id == 3 && falling),
            0,
            0,
            0,
            0,
            0,
        ] {
            word(&mut b, v);
        }
    }
    let faces = [
        [[0., 0., 1.], [0., 0., 0.], [1., 0., 0.], [1., 0., 1.]],
        [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]],
        [[1., 0., 0.], [0., 0., 0.], [0., 1., 0.], [1., 1., 0.]],
        [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
        [[0., 0., 0.], [0., 0., 1.], [0., 1., 1.], [0., 1., 0.]],
        [[1., 0., 1.], [1., 0., 0.], [1., 1., 0.], [1., 1., 1.]],
    ];
    for id in 1..=2u32 {
        for v in [id, 1, 6] {
            word(&mut b, v);
        }
        for (face, p) in faces.into_iter().enumerate() {
            for v in [
                face as u32,
                if id == 2 { 0 } else { u32::MAX },
                if id == 2 { 1 } else { 0 },
                1,
                0,
            ] {
                word(&mut b, v);
            }
            floats(&mut b, p.into_iter().flatten());
            floats(
                &mut b,
                [[0., 0.], [0.5, 0.], [0.5, 1.], [0., 1.]]
                    .into_iter()
                    .flatten(),
            );
        }
    }
    for v in [1, 2, 1, 2, 2, 0] {
        word(&mut b, v);
    }
    for id in 1..=2u32 {
        word(&mut b, id);
        text(&mut b, &format!("fixture:sprite_{id}"));
        floats(
            &mut b,
            if id == 1 {
                [0., 0., 0.5, 1.]
            } else {
                [0.5, 0., 1., 1.]
            },
        );
        for v in [16, 16, 0, 0, 1, 16, 16, 0, 0, 0] {
            word(&mut b, v);
        }
    }
    b
}

/// 通过真实PRM1解码和资源准备得到可编译Catalog；GPU不参与此CPU fixture。
pub fn catalog() -> Result<Catalog, Error> {
    let mut catalog = Catalog::default();
    {
        let mut prepared = catalog.stage_resources(&resource_packet())?;
        let maps = prepared.material_resources([32, 16], &[160, 200, 100, 255].repeat(32 * 16))?;
        drop(maps);
        prepared.commit();
    }
    Ok(catalog)
}

/// 生成generation1的PRT3响应；state palette按源index=(y*16+z)*16+x编码。
pub fn section_packet(records: &[(u64, [i32; 3], Pattern)]) -> Vec<u8> {
    let mut b = Vec::new();
    word(&mut b, 0x33545250);
    word(&mut b, records.len() as u32);
    long(&mut b, 1);
    for &(ticket, key, pattern) in records {
        long(&mut b, ticket);
        for v in key {
            word(&mut b, v as u32);
        }
        let state = match pattern {
            Pattern::Air => 0,
            Pattern::Dense | Pattern::Sparse => 1,
            Pattern::Fluid => 3,
            Pattern::Tinted => 2,
        };
        let sparse = matches!(pattern, Pattern::Sparse);
        for v in [
            1,
            u32::from(sparse),
            if sparse { 2 } else { 1 },
            if sparse { 64 } else { 0 },
        ] {
            word(&mut b, v);
        }
        if sparse {
            word(&mut b, 0);
            word(&mut b, state);
            let mut words = [0u64; 64];
            for index in 0..4096usize {
                let x = index % 16;
                let z = index / 16 % 16;
                let y = index / 256;
                if x % 4 == 0 && y % 4 == 0 && z % 4 == 0 {
                    words[index / 64] |= 1u64 << (index % 64);
                }
            }
            for value in words {
                long(&mut b, value);
            }
        } else {
            word(&mut b, state);
        }
    }
    b
}
