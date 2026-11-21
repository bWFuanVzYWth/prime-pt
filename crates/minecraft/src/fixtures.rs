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

/// 完整模型/PTR1保持同一指定世代的资源fixture，用于真实替换事务。
pub fn resource_packet_generation(generation: u64) -> Vec<u8> {
    resource_packet_fields(0, false, generation)
}

pub(crate) fn resource_packet_with_fluid_fields(support: u32, falling: bool) -> Vec<u8> {
    resource_packet_fields(support, falling, 1)
}
fn resource_packet_fields(support: u32, falling: bool, generation: u64) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [0x314d5250, 263] {
        word(&mut b, v);
    }
    long(&mut b, generation);
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
    let mut tint = crate::tint::fixtures::resource_bytes();
    tint[8..16].copy_from_slice(&generation.to_le_bytes());
    // fixture水状态为3，保留真实流体-1来源；不是缺recipe时默认白色。
    let count_at = 16 + 256 * 4 + 16 + 3 * 4;
    tint[count_at..count_at + 4].copy_from_slice(&8u32.to_le_bytes());
    for value in [3, u32::MAX, 4, 0] {
        word(&mut tint, value);
    }
    word(&mut b, tint.len() as u32);
    b.extend(tint);
    b
}

/// 以指定原版方块与同名 sprite 替换状态 1 的六面模型材质，使用实际内嵌预设。
/// 图集为 32×16，左半部由调用方填入底色；错误来自原始资源字典的校验。
pub fn vanilla_cube<'a>(
    catalog: &'a mut Catalog,
    name: &str,
) -> Result<crate::PreparedCatalog<'a>, Error> {
    let mut prepared = catalog.stage_resources(&resource_packet())?;
    prepared.next.states.get_mut(&1).unwrap().name = format!("minecraft:{name}");
    let sprite = prepared.next.sprites.get_mut(&1).unwrap();
    sprite.name = format!("minecraft:block/{name}");
    sprite.vanilla = true;
    Ok(prepared)
}

/// 26.3 FaceBakery 实测的贴地地衣。两种源面顺序用于核对外露面的选择。
/// 候选使用 32×16 图集左半部；调用方提供颜色和 alpha，然后提交资源世代。
pub fn floor_lichen(
    catalog: &mut Catalog,
    reverse_order: bool,
) -> Result<crate::PreparedCatalog<'_>, Error> {
    use crate::resources::{Model, SourceQuad, State};
    let mut prepared = vanilla_cube(catalog, "glow_lichen")?;
    let state = prepared.next.states.get_mut(&1).unwrap();
    state.flags |= State::MULTIFACE;
    state.emission = 7;
    // 26.3 FaceBakery 的 90° UV 锁定变换。背面纹理在同一位置旋转了 180°。
    let quad = |xz: [[f32; 2]; 4], uv: [[f32; 2]; 4]| SourceQuad {
        face: 6,
        tint: -1,
        layer: 1,
        sprite: 1,
        emission: 0,
        positions: xz.map(|[x, z]| [x, 0.00625, z]),
        uv: uv.map(|[u, v]| [0.5 * u, v]),
    };
    let back = quad(
        [[0., 1.], [0., 0.], [1., 0.], [1., 1.]],
        [[1., 0.], [1., 1.], [0., 1.], [0., 0.]],
    );
    let front = quad(
        [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
        [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
    );
    let mut faces = vec![back, front];
    if reverse_order {
        faces.reverse();
    }
    prepared.next.models.insert(1, Model::Mesh(faces));
    Ok(prepared)
}

/// 原版草方块的四张共面侧面和染色源；可为底色及覆盖层各提供一对 LabPBR 法线、参数图。
/// 图集固定为 32×16，底色在左，覆盖在右。返回尚未准备 GPU 材质的候选世代。
pub fn grass_block(
    catalog: &mut Catalog,
    lab: Option<[(crate::ImageSource, crate::ImageSource); 2]>,
) -> Result<crate::PreparedCatalog<'_>, Error> {
    use crate::resources::{Model, SourceQuad};
    let mut prepared = catalog.stage_resources(&resource_packet())?;
    prepared.next.states.get_mut(&2).unwrap().name = "minecraft:grass_block".into();
    let layers = lab.map_or([None, None], |pair| pair.map(Some));
    for (index, (name, sources)) in ["grass_block_side", "grass_block_side_overlay"]
        .into_iter()
        .zip(layers)
        .enumerate()
    {
        let sprite = prepared.next.sprites.get_mut(&(index as u32 + 1)).unwrap();
        sprite.name = format!("minecraft:block/{name}");
        sprite.vanilla = sources.is_none();
        if let Some((normal, specular)) = sources {
            let map = |image: crate::ImageSource| {
                if !(1..=16384).contains(&image.width)
                    || !(1..=16384).contains(&image.height)
                    || u64::from(image.width) * u64::from(image.height) * 4
                        != image.pixels.len() as u64
                {
                    return Err(Error::Input("草方块测试材质需要完整的非空 RGBA8 图像"));
                }
                crate::lab_pbr::MaterialImage::inherited(
                    image,
                    &sprite.images[0],
                    sprite.extent,
                    &sprite.frames,
                    sprite.interpolate,
                )
            };
            sprite.normal = Some(map(normal)?);
            sprite.specular = Some(map(specular)?);
        }
    }
    let Model::Mesh(quads) = prepared.next.models.get_mut(&2).unwrap() else {
        unreachable!("固定测试模型")
    };
    let base = std::mem::take(quads);
    for mut q in base {
        if (2..6).contains(&q.face) {
            // 四个侧面与不同顶点起点，覆盖层先于底层。
            let first = q.face as usize % 4;
            quads.push(SourceQuad {
                positions: std::array::from_fn(|i| q.positions[(i + first) % 4]),
                uv: std::array::from_fn(|i| {
                    let [u, v] = q.uv[(i + first) % 4];
                    [u + 0.5, v]
                }),
                sprite: 2,
                ..q
            });
            q.layer = 0;
            q.tint = -1;
        }
        quads.push(q);
    }
    Ok(prepared)
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
