//! 宿主变化包的完整边界校验；解析不触碰世界或资源所有者。
use crate::Error;
use prime_world::SourceChange;

pub(super) fn decode(bytes: &[u8]) -> Result<Vec<SourceChange>, Error> {
    if bytes.len() < 16
        || word(bytes, 0) != 0x3149_4450
        || word(bytes, 4) != 1
        || word(bytes, 12) != 0
    {
        return Err(Error::Input("PDI1头无效"));
    }
    let count = word(bytes, 8) as usize;
    if count > 65_536 || count.checked_mul(24).and_then(|n| n.checked_add(16)) != Some(bytes.len())
    {
        return Err(Error::Input("PDI1数量或长度无效"));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| Error::Input("变化批容量不足"))?;
    for record in bytes[16..].as_chunks::<24>().0 {
        let kind = word(record, 0);
        let key = [4, 8, 12].map(|at| word(record, at) as i32);
        let a = word(record, 16);
        let b = word(record, 20);
        if key.iter().any(|v| v.checked_mul(16).is_none()) {
            return Err(Error::Input("变化坐标越界"));
        }
        let change = match kind {
            1 => {
                if (a | b) & !0xfff != 0 {
                    return Err(Error::Input("block范围保留位非零"));
                }
                let minimum = [0, 8, 4].map(|s| ((a >> s) & 15) as u8);
                let maximum = [0, 8, 4].map(|s| (((b >> s) & 15) + 1) as u8);
                if minimum.iter().zip(maximum).any(|(&lo, hi)| lo >= hi) {
                    return Err(Error::Input("block范围为空或反向"));
                }
                SourceChange::BlockRange {
                    section: key,
                    minimum,
                    maximum,
                }
            }
            2 if a == 0 && b == 0 => SourceChange::SectionSource(key),
            3 => {
                let maximum_y = a as i32;
                if b == 0
                    || b & !3 != 0
                    || maximum_y <= key[1]
                    || i64::from(maximum_y) - i64::from(key[1]) > 4096
                    || maximum_y.checked_mul(16).is_none()
                {
                    return Err(Error::Input("列变化高度或标记无效"));
                }
                SourceChange::ColumnSource {
                    x: key[0],
                    z: key[2],
                    minimum_y: key[1],
                    maximum_y,
                    geometry: b & 1 != 0,
                    biome: b & 2 != 0,
                }
            }
            4 if key[1] == 0 && a == 0 && b == 0 => SourceChange::BiomeColumn {
                x: key[0],
                z: key[2],
            },
            5 if key == [0; 3] && a == 0 && b == 0 => SourceChange::WorldTint,
            6 if key == [0; 3] && a == 0 && b == 0 => SourceChange::SourceOverflow,
            _ => return Err(Error::Input("变化类型或保留字段无效")),
        };
        output.push(change);
    }
    Ok(output)
}
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("上方已校验固定布局"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(records: &[[u32; 6]]) -> Vec<u8> {
        [0x31494450, 1, records.len() as u32, 0]
            .into_iter()
            .chain(records.iter().flatten().copied())
            .flat_map(u32::to_le_bytes)
            .collect()
    }
    #[test]
    fn inclusive_local_wire_corners_become_half_open_xyz_bounds() {
        let changes = decode(&packet(&[[1, 2, (-3i32) as u32, 4, 0x210, 0xfff]])).unwrap();
        assert_eq!(
            changes,
            [SourceChange::BlockRange {
                section: [2, -3, 4],
                minimum: [0, 2, 1],
                maximum: [16; 3]
            }]
        );
    }
    #[test]
    fn invalid_tail_rejects_the_entire_batch_before_any_owner_is_accessed() {
        assert!(decode(&packet(&[[2, 1, 2, 3, 0, 0], [5, 0, 0, 0, 1, 0]])).is_err());
        let mut trailing = packet(&[]);
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }
}
