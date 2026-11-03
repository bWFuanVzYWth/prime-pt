//! tint纯内核的代表性资源/世界fixture；无宿主最终颜色调用。
use super::{TintResources, TintWorld};
use crate::{Error, TintRequest};
fn u32b(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u64b(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
/// 世代1：state1草、state2草(slot0)/水(slot-1)、state3满能红石、state4自定义、state5固定色、state6上半高草。
pub fn resources() -> Result<TintResources, Error> {
    TintResources::decode(&resource_bytes())
}
/// PTR1完整资源原包，可用于生产decode/事务测试。
pub fn resource_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    u32b(&mut b, 0x31525450);
    u32b(&mut b, 263);
    u64b(&mut b, 1);
    for i in 0..256 {
        u32b(&mut b, i);
    }
    for _ in 0..4 {
        u32b(&mut b, 0);
    }
    for _ in 0..3 {
        u32b(&mut b, 0);
    }
    let recipes = [
        (1, 0, 1, 0),
        (2, -1, 4, 0),
        (2, 0, 1, 0),
        (3, 0, 6, 15),
        (4, 0, 9, 0),
        (5, 0, 8, 0x80446688),
        (6, 0, 5, 1),
    ];
    u32b(&mut b, recipes.len() as u32);
    for (state, slot, kind, value) in recipes {
        for v in [state, slot as u32, kind, value] {
            u32b(&mut b, v);
        }
    }
    b
}
/// seed0x123456789abcdef；palette0固定草0xff336633/水0xff224488，palette1为另一组源色。
pub fn world(resources: &TintResources) -> Result<TintWorld, Error> {
    TintWorld::decode(&world_bytes(), resources)
}
/// PBW1完整世界原包，可用于生产decode/事务测试。
pub fn world_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    u32b(&mut b, 0x31574250);
    u32b(&mut b, 263);
    u64b(&mut b, 1);
    u64b(&mut b, 0x123456789abcdef);
    u32b(&mut b, 2);
    for c in [
        [0xff336633, 0xff557711, 0xff667788, 0xff224488],
        [0xff99aa44, 0xffbbcc55, 0xffddeeff, 0xff663311],
    ] {
        for v in [
            0.8f32.to_bits(),
            0.7f32.to_bits(),
            c[3],
            c[0],
            c[1],
            c[2],
            7,
            0,
        ] {
            u32b(&mut b, v);
        }
    }
    b
}
/// 完整16²草色平面；y64，半径0/7均能以palette0得到0xff336633。
pub fn requests() -> Vec<TintRequest> {
    (0..16)
        .flat_map(|z| {
            (0..16).map(move |x| TintRequest {
                position: [x, 64, z],
                state: 1,
                slot: 0,
            })
        })
        .collect()
}
