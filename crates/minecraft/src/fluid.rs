//! 原版水/岩浆从实际27-cell邻域生成斜顶与侧面；不在Java运行tessellator。
//!
//! 原版FlowingFluid.getFlow的falling支撑分支只正比例缩放已归一化XZ，并添加Y分量；
//! 此编译器只消费XZ是否为零与atan2方向，不消费Y或速度幅度。保留缩放前的1e-5阈值，
//! 省略不改变这些消费者的支撑求值与缩放；不将动态支撑未知伪装为false。

use crate::{
    Catalog, Error,
    compile::{CompiledSlab, Neighborhood, fluid_state},
    resources::{Placement, State},
};
use prime_scene::Quad;

static EMPTY: State = State {
    flags: 1,
    model: 0,
    name: String::new(),
    faces: [0; 6],
    fluid_kind: 0,
    fluid: 0,
    fluid_tint: None,
    level: 0,
    emission: 0,
    placement: Placement {
        kind: 0,
        horizontal: 0.,
        vertical: 0.,
        seed: 0,
    },
    face_mask: 0,
    matching_block: 0,
    blocks_motion: false,
    leaves: Vec::new(),
};

fn height(state: &State) -> f32 {
    if state.fluid_kind == 0 {
        0.
    } else {
        (if state.level == 0 { 8 } else { state.level }) as f32 / 9.
    }
}

fn corner(center: f32, a: f32, b: f32, diagonal: f32) -> f32 {
    if a >= 1. || b >= 1. {
        return 1.;
    }
    let mut total = 0.;
    let mut weight = 0.;
    let mut add = |v: f32| {
        if v >= 0. {
            let w = if v >= 0.8 { 10. } else { 1. };
            total += v * w;
            weight += w;
        }
    };
    if a > 0. || b > 0. {
        if diagonal >= 1. {
            return 1.;
        }
        add(diagonal);
    }
    add(center);
    add(b);
    add(a);
    if weight == 0. { 0. } else { total / weight }
}

fn flow_angle(flow: [f64; 2]) -> Option<f32> {
    (flow != [0.; 2]).then(|| flow[1].atan2(flow[0]) as f32 - std::f32::consts::FRAC_PI_2)
}

fn occluded(catalog: &Catalog, state: &State, face: usize, height: f32) -> bool {
    let id = state.faces[face];
    if id == 1 {
        return height <= 1.;
    }
    if id == 0 {
        return false;
    }
    let Some(shape) = catalog.faces.get(&id) else {
        return false;
    };
    if shape.u[0] > 0. || shape.u[shape.u.len() - 1] < 1. || shape.v[0] > 0. {
        return false;
    }
    for u in 0..shape.u.len() - 1 {
        for v in 0..shape.v.len() - 1 {
            if shape.u[u + 1] <= 0. || shape.u[u] >= 1. || shape.v[v] >= f64::from(height) {
                continue;
            }
            let bit = u * (shape.v.len() - 1) + v;
            if shape.words[bit / 64] & (1u64 << (bit % 64)) == 0 {
                return false;
            }
        }
    }
    shape.v[shape.v.len() - 1] >= f64::from(height)
}

pub(crate) struct FluidBlock<'a> {
    pub state: &'a State,
    pub state_id: u32,
    pub local: [i32; 3],
    pub origin: [f32; 3],
}

pub(crate) fn emit(
    catalog: &Catalog,
    neighborhood: &Neighborhood<'_>,
    block: FluidBlock<'_>,
    output: &mut CompiledSlab,
    max_quads: usize,
) -> Result<(), Error> {
    let FluidBlock {
        state,
        state_id,
        local,
        origin,
    } = block;
    if state.fluid_kind > 2 {
        return Err(Error::Input("自定义流体需要显式源适配"));
    }
    let source = catalog
        .fluids
        .get(&state.fluid)
        .ok_or(Error::Input("未准备的流体材质"))?;
    let mut cells = [&EMPTY; 27];
    for dy in -1..=1 {
        for dz in -1..=1 {
            for dx in -1..=1 {
                cells[((dy + 1) * 9 + (dz + 1) * 3 + dx + 1) as usize] = fluid_state(
                    catalog,
                    neighborhood,
                    [local[0] + dx, local[1] + dy, local[2] + dz],
                )?
                .unwrap_or(&EMPTY);
            }
        }
    }
    let get = |dx: i32, dy: i32, dz: i32| cells[((dy + 1) * 9 + (dz + 1) * 3 + dx + 1) as usize];
    let directions = [
        [0, -1, 0],
        [0, 1, 0],
        [0, 0, -1],
        [0, 0, 1],
        [-1, 0, 0],
        [1, 0, 0],
    ];
    let neighbors = directions.map(|[x, y, z]| get(x, y, z));
    let kind = state.fluid_kind;
    let mut enabled = std::array::from_fn::<_, 6, _>(|face| {
        neighbors[face].fluid_kind != kind && (face == 1 || !occluded(catalog, state, face ^ 1, 1.))
    });
    enabled[0] &= !occluded(catalog, neighbors[0], 0, 0.8888889);
    let samples = std::array::from_fn::<_, 9, _>(|i| {
        let x = i as i32 % 3 - 1;
        let z = i as i32 / 3 - 1;
        let s = get(x, 0, z);
        if s.fluid_kind == kind {
            if get(x, 1, z).fluid_kind == kind {
                1.
            } else {
                height(s)
            }
        } else if s.flags & 32 != 0 {
            -1.
        } else {
            0.
        }
    });
    let mut flow = [0f64; 2];
    for (x, z) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
        let neighbor = get(x, 0, z);
        if neighbor.fluid_kind != 0 && neighbor.fluid_kind != kind {
            continue;
        }
        let mut h = height(neighbor);
        let mut distance = 0.;
        if h == 0. {
            if !neighbor.blocks_motion {
                let below = get(x, -1, z);
                if below.fluid_kind == 0 || below.fluid_kind == kind {
                    h = height(below);
                    if h > 0. {
                        distance = height(state) - (h - 0.8888889);
                    }
                }
            }
        } else {
            distance = height(state) - h;
        }
        flow[0] += f64::from(x as f32 * distance);
        flow[1] += f64::from(z as f32 * distance);
    }
    let length = (flow[0] * flow[0] + flow[1] * flow[1]).sqrt();
    if length < 1e-5 {
        flow = [0.; 2];
    } else {
        flow = flow.map(|v| v / length);
    }
    let mut heights = [
        corner(samples[4], samples[1], samples[3], samples[0]),
        corner(samples[4], samples[7], samples[3], samples[6]),
        corner(samples[4], samples[7], samples[5], samples[8]),
        corner(samples[4], samples[1], samples[5], samples[2]),
    ];
    enabled[1] &= !occluded(
        catalog,
        neighbors[1],
        1,
        heights.into_iter().fold(1., f32::min),
    );
    let bottom = if enabled[0] { 0.001 } else { 0. };
    let mut emit =
        |positions: [[f32; 3]; 4], uv: [[f32; 2]; 4], which: usize| -> Result<(), Error> {
            let sprite = source.sprites[which];
            let bounds = catalog
                .sprites
                .get(&sprite)
                .ok_or(Error::Input("流体sprite未准备"))?
                .bounds;
            let uv = uv.map(|uv| {
                [
                    bounds[0] + (bounds[2] - bounds[0]) * uv[0],
                    bounds[1] + (bounds[3] - bounds[1]) * uv[1],
                ]
            });
            let material = catalog
                .fluid_materials
                .get(&(state_id, which))
                .copied()
                .ok_or(Error::Input("流体材质尚未准备"))?;
            let at = output.mesh.quads.len();
            if at >= max_quads {
                return Err(Error::OutputLimit {
                    required: at.saturating_add(1),
                });
            }
            output.mesh.quads.push(Quad::with_material(
                positions.map(|p| {
                    std::array::from_fn(|axis| p[axis] + local[axis] as f32 + origin[axis])
                }),
                uv,
                [1.; 3],
                material,
            ));
            if source.tinted {
                output.bind(
                    at,
                    state.fluid_tint.ok_or(Error::Input("流体tint来源未准备"))?,
                    local,
                );
            }
            Ok(())
        };
    if enabled[1] {
        for h in &mut heights {
            *h -= 0.001;
        }
        let (uv, sprite) = if let Some(angle) = flow_angle(flow) {
            let (s, c) = angle.sin_cos();
            let (s, c) = (s * 0.25, c * 0.25);
            (
                [
                    [0.5 - c - s, 0.5 - c + s],
                    [0.5 - c + s, 0.5 + c + s],
                    [0.5 + c + s, 0.5 + c - s],
                    [0.5 + c - s, 0.5 - c - s],
                ],
                1,
            )
        } else {
            ([[0., 0.], [0., 1.], [1., 1.], [1., 0.]], 0)
        };
        emit(
            [
                [0., heights[0], 0.],
                [0., heights[1], 1.],
                [1., heights[2], 1.],
                [1., heights[3], 0.],
            ],
            uv,
            sprite,
        )?;
    }
    if enabled[0] {
        emit(
            [
                [0., bottom, 0.],
                [1., bottom, 0.],
                [1., bottom, 1.],
                [0., bottom, 1.],
            ],
            [[0., 0.], [1., 0.], [1., 1.], [0., 1.]],
            0,
        )?;
    }
    for (direction, a, b, x0, z0, x1, z1) in [
        (2, 0, 3, 0., 0.001, 1., 0.001),
        (3, 2, 1, 1., 0.999, 0., 0.999),
        (4, 1, 0, 0.001, 1., 0.001, 0.),
        (5, 3, 2, 0.999, 0., 0.999, 1.),
    ] {
        let (ha, hb) = (heights[a], heights[b]);
        if !enabled[direction] || occluded(catalog, neighbors[direction], direction, ha.max(hb)) {
            continue;
        }
        let overlay = source.sprites[2] != 0 && neighbors[direction].flags & 64 != 0;
        let sprite = if overlay { 2 } else { 1 };
        emit(
            [
                [x0, ha, z0],
                [x1, hb, z1],
                [x1, bottom, z1],
                [x0, bottom, z0],
            ],
            [
                [0., (1. - ha) * 0.5],
                [0.5, (1. - hb) * 0.5],
                [0.5, 0.5],
                [0., 0.5],
            ],
            sprite,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::flow_angle;
    use crate::{
        Catalog, Neighbor, Neighborhood, compile_section_slab, decode_sections,
        fixtures::{self, Pattern},
    };
    #[test]
    fn falling_vertical_normalization_does_not_change_uv_direction_or_still_selection() {
        assert_eq!(flow_angle([0., 0.]), None);
        for x in -16..=16 {
            for z in -16..=16 {
                if x == 0 && z == 0 {
                    continue;
                }
                let length = f64::from(x * x + z * z).sqrt();
                let flow = [f64::from(x) / length, f64::from(z) / length];
                let legacy_length = (flow[0] * flow[0] + 36. + flow[1] * flow[1]).sqrt();
                let legacy = flow.map(|v| v / legacy_length);
                assert!(
                    (flow_angle(flow).unwrap() - flow_angle(legacy).unwrap()).abs()
                        <= 2. * f32::EPSILON
                );
            }
        }
    }

    #[test]
    fn actual_fluid_mesh_ignores_static_or_unknown_support_and_falling_wire_fields() {
        let sources = decode_sections(&fixtures::section_packet(&[
            (1, [0; 3], Pattern::Fluid),
            (2, [1, 0, 0], Pattern::Dense),
            (3, [-1, 0, 0], Pattern::Tinted),
        ]))
        .unwrap();
        let mut sections = [Neighbor::Empty; 27];
        sections[13] = Neighbor::Available(sources[0].storage.as_ref().unwrap());
        sections[14] = Neighbor::Available(sources[1].storage.as_ref().unwrap());
        sections[12] = Neighbor::Available(sources[2].storage.as_ref().unwrap());
        let mut reference = None;
        for support in [0, 0x3ffff, 0x80000000] {
            for falling in [false, true] {
                let mut catalog = Catalog::default();
                let mut prepared = catalog
                    .stage_resources(&fixtures::resource_packet_with_fluid_fields(
                        support, falling,
                    ))
                    .unwrap();
                drop(
                    prepared
                        .material_resources([32, 16], &[160, 200, 100, 255].repeat(32 * 16))
                        .unwrap(),
                );
                prepared.commit();
                let shallow = catalog.states.get_mut(&1).unwrap();
                shallow.flags = 16;
                shallow.model = 0;
                shallow.fluid_kind = 1;
                shallow.fluid = 1;
                shallow.level = 4;
                for which in 0..2 {
                    catalog
                        .fluid_materials
                        .insert((1, which), catalog.fluid_materials[&(3, which)]);
                }
                let output = compile_section_slab(
                    &catalog,
                    &Neighborhood {
                        key: [0; 3],
                        generation: 1,
                        sections,
                    },
                    12,
                )
                .unwrap();
                assert!(
                    output
                        .mesh
                        .quads
                        .iter()
                        .any(|quad| quad.positions().iter().all(|p| p[1] > 0.)
                            && quad.uv().iter().all(|uv| uv[0] > 0.5 && uv[0] < 1.))
                );
                let actual: Vec<_> = output
                    .mesh
                    .quads
                    .iter()
                    .map(|quad| (quad.positions(), quad.attributes()))
                    .collect();
                if let Some(reference) = &reference {
                    assert_eq!(&actual, reference);
                } else {
                    reference = Some(actual);
                }
            }
        }
    }
}
