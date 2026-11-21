//! 在资源准备时解析模型的所有可达面，区分附着薄片的外露面与支撑面。
use crate::resources::{Model, SourceQuad};
use std::collections::{HashMap, HashSet};

pub(crate) struct ModelFaces<'a> {
    pub(crate) meshes: Vec<(u32, &'a [SourceQuad])>,
    pub(crate) unknown: bool,
    pub(crate) planar: bool,
    pub(crate) mapped_emission: bool,
}

impl<'a> ModelFaces<'a> {
    pub(crate) fn collect(
        root: u32,
        models: &'a HashMap<u32, Model>,
        emits: impl Fn(u32) -> bool,
    ) -> Self {
        let mut meshes = Vec::new();
        let mut unknown = false;
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if id == 0 || !seen.insert(id) {
                continue;
            }
            match &models[&id] {
                Model::Unknown => unknown = true,
                Model::Mesh(quads) => meshes.push((id, quads.as_slice())),
                Model::Alias(child) => stack.push(*child),
                Model::Multipart(children) => stack.extend(children.iter().rev()),
                Model::Weighted { children, .. } => {
                    stack.extend(children.iter().rev().map(|&(_, child)| child));
                }
            }
        }
        let mut quads = meshes.iter().flat_map(|&(_, quads)| quads);
        let planar = !unknown
            && quads
                .clone()
                .find_map(plane)
                .is_some_and(|(origin, normal)| {
                    quads.clone().all(|q| {
                        q.positions.iter().all(|&p| {
                            let delta =
                                std::array::from_fn::<_, 3, _>(|i| f64::from(p[i]) - origin[i]);
                            normal[0] * delta[0] + normal[1] * delta[1] + normal[2] * delta[2] == 0.
                        })
                    })
                });
        let mapped_emission = quads.any(|q| emits(q.sprite));
        Self {
            meshes,
            unknown,
            planar,
            mapped_emission,
        }
    }
}

fn plane(q: &SourceQuad) -> Option<([f64; 3], [f64; 3])> {
    let origin = q.positions[0].map(f64::from);
    let edges = [1, 2, 3]
        .map(|j| std::array::from_fn::<_, 3, _>(|i| f64::from(q.positions[j][i]) - origin[i]));
    edges.windows(2).find_map(|pair| {
        let [a, b] = [pair[0], pair[1]];
        let n = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        n.iter().any(|&v| v != 0.).then_some((origin, n))
    })
}

fn exact_sum(a: f64, b: f64) -> (f64, f64) {
    let sum = a + b;
    let virtual_b = sum - a;
    (sum, (a - (sum - virtual_b)) + (b - virtual_b))
}

pub(crate) fn affine_quad<const N: usize>(corners: &[[f32; N]; 4]) -> bool {
    (0..N).all(|axis| {
        exact_sum(f64::from(corners[0][axis]), f64::from(corners[2][axis]))
            == exact_sum(f64::from(corners[1][axis]), f64::from(corners[3][axis]))
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    Single,
    Front,
    Both,
    Omit,
}

/// 宿主附着层的外侧朝向本格内部。只接管轴对齐、同材质的共面镂空对。
fn faces_cell_center(q: &SourceQuad) -> Option<bool> {
    let (origin, normal) = plane(q)?;
    let axis = (0..3)
        .find(|&i| normal[i] != 0. && q.positions.iter().all(|p| p[i] == q.positions[0][i]))?;
    let distance = 0.5 - origin[axis];
    (distance != 0. && (0. ..=1.).contains(&origin[axis])).then(|| normal[axis] * distance > 0.)
}

pub(crate) fn bilateral(quads: &[SourceQuad], attached: bool) -> Vec<Side> {
    let mut sides = vec![Side::Single; quads.len()];
    for (a, first) in quads.iter().enumerate() {
        if sides[a] != Side::Single {
            continue;
        }
        for (b, second) in quads.iter().enumerate().skip(a + 1) {
            if sides[b] != Side::Single {
                continue;
            }
            let mapping = [0usize, 2, 1, 3].into_iter().find_map(|shift| {
                let map = std::array::from_fn::<_, 4, _>(|i| (shift + 4 - i) % 4);
                (shift % 2 == 0 || affine_quad(&first.positions)).then_some(())?;
                (0..4)
                    .all(|i| first.positions[i] == second.positions[map[i]])
                    .then_some((map, shift))
            });
            let Some((map, shift)) = mapping else {
                continue;
            };
            sides[a] = Side::Front;
            sides[b] = Side::Front;
            if first.face == second.face
                && first.tint == second.tint
                && first.layer == second.layer
                && first.sprite == second.sprite
                && first.emission == second.emission
            {
                let identical = (shift % 2 == 0 || affine_quad(&first.uv))
                    && (0..4).all(|i| first.uv[i] == second.uv[map[i]]);
                let exposed = (attached && first.layer == 1)
                    .then(|| faces_cell_center(first))
                    .flatten();
                if let Some(keep_first) = exposed.or(identical.then_some(true)) {
                    let (keep, omit) = if keep_first { (a, b) } else { (b, a) };
                    sides[keep] = Side::Both;
                    sides[omit] = Side::Omit;
                }
            }
            break;
        }
    }
    sides
}
