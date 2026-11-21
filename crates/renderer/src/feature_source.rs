//! 本帧的原版网格、来源身份和纹理借用。按 span 校验布局，顶点在宿主字节中原位读取。
use super::{Error, scene_cache};
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;

pub(super) const WHITE: u32 = u32::MAX;
pub(super) const BLOCK_ATLAS: u32 = u32::MAX - 1;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub(super) struct Texture {
    pub view: u64,
    pub identity: u64,
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub srgb: u32,
    pub reserved: u32,
}

#[derive(Clone, Copy)]
pub(super) struct Vertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: u32,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) struct FaceKey {
    pub owner: u64,
    pub part: u32,
    pub primitive: u32,
    pub texture: u64,
    pub flags: u32,
}
#[derive(Clone)]
pub(super) struct Face {
    pub key: FaceKey,
    pub corners: [Vertex; 4],
    pub triangle: bool,
    pub emission: (u32, f32),
    pub texture: u32,
}
pub(super) struct Model {
    pub key: FaceKey,
    pub bone: u64,
    pub texture: u32,
    pub index: u32,
    pub color: u32,
    pub transform: [[f32; 4]; 3],
}
pub(super) struct Source {
    pub models: Vec<Model>,
    pub origin: [f64; 3],
    pub textures: Vec<Texture>,
    pub faces: Vec<Face>,
}

pub(super) fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], Error> {
    let (a, b) = input
        .split_at_checked(len)
        .ok_or(Error::Invalid("动态网格源包截断"))?;
    *input = b;
    Ok(a)
}
pub(super) fn read<T: Pod>(input: &mut &[u8]) -> Result<T, Error> {
    Ok(bytemuck::pod_read_unaligned(take(input, size_of::<T>())?))
}
impl Source {
    pub fn decode(
        mut bytes: &[u8],
        items: Option<&prime_scene::ItemMaterials>,
    ) -> Result<Self, Error> {
        let [tag, texture_count, span_count, _] = read::<[u32; 4]>(&mut bytes)?;
        if tag != u32::from_le_bytes(*b"PTF1") || texture_count > 16384 || span_count > 2_000_000 {
            return Err(Error::Invalid("动态帧标记或源数量无效"));
        }
        let origin: [f64; 3] = read(&mut bytes)?;
        if !origin.iter().all(|n| n.is_finite() && n.abs() < 6e7) {
            return Err(Error::Invalid("动态网格相机原点无效"));
        }
        let mut textures = Vec::new();
        textures
            .try_reserve_exact(texture_count as usize)
            .map_err(scene_cache::Error::from)?;
        for _ in 0..texture_count {
            let t: Texture = read(&mut bytes)?;
            if t.identity == 0
                || t.view == 0
                || t.srgb > 1
                || t.width == 0
                || t.height == 0
                || t.width > 16384
                || t.height > 16384
            {
                return Err(Error::Invalid("动态纹理视图无效"));
            }
            textures.push(t);
        }
        let mut models = Vec::new();
        let mut faces = Vec::new();
        for _ in 0..span_count {
            let owner = read::<u64>(&mut bytes)?;
            let [
                part,
                flags,
                texture,
                topology,
                count,
                stride,
                pos,
                color,
                uv,
            ] = read::<[u32; 9]>(&mut bytes)?;
            let tid = match texture {
                WHITE | BLOCK_ATLAS => u64::from(texture),
                i => {
                    textures
                        .get(i as usize)
                        .ok_or(Error::Invalid("动态纹理索引无效"))?
                        .identity
                }
            };
            if topology == 0 {
                if count != 1 || stride != 64 || flags & !63 != 0 {
                    return Err(Error::Invalid("模型实例布局无效"));
                }
                let bone = read::<u64>(&mut bytes)?;
                let index = read::<u32>(&mut bytes)?;
                let argb = read::<u32>(&mut bytes)?;
                let transform: [[f32; 4]; 3] = read(&mut bytes)?;
                if !transform.iter().flatten().all(|v| v.is_finite()) {
                    return Err(Error::Invalid("模型实例姿态非有限"));
                }
                let color = (argb & 0xff00ff00) | ((argb >> 16) & 255) | ((argb & 255) << 16);
                models.push(Model {
                    bone,
                    key: FaceKey {
                        owner,
                        part,
                        primitive: index,
                        texture: tid,
                        flags,
                    },
                    texture,
                    index,
                    color,
                    transform,
                });
                continue;
            }
            if topology == 1 {
                if stride != 64 || flags & !3 != 0 || faces.len() + count as usize > 2_000_000 {
                    return Err(Error::Invalid("粒子源布局或实例数无效"));
                }
                let data = take(&mut bytes, count as usize * 64)?;
                faces
                    .try_reserve(count as usize)
                    .map_err(scene_cache::Error::from)?;
                for mut item in data.chunks_exact(64) {
                    let particle = read::<u64>(&mut item)?;
                    let v = read::<[f32; 12]>(&mut item)?;
                    let argb = read::<u32>(&mut item)?;
                    let light = read::<u32>(&mut item)?;
                    if !v.iter().all(|n| n.is_finite()) {
                        return Err(Error::Invalid("粒子姿态含非有限值"));
                    }
                    let [x, y, z, i, j, k, w, s, u0, u1, v0, v1] = v;
                    let right = [
                        w * w + i * i - j * j - k * k,
                        2. * (i * j + k * w),
                        2. * (i * k - j * w),
                    ];
                    let up = [
                        2. * (i * j - k * w),
                        w * w - i * i + j * j - k * k,
                        2. * (j * k + i * w),
                    ];
                    let center = [x, y, z];
                    let uv = [[u1, v1], [u1, v0], [u0, v0], [u0, v1]];
                    let corners = std::array::from_fn(|n| {
                        let [dx, dy] = [[1., -1.], [1., 1.], [-1., 1.], [-1., -1.]][n];
                        Vertex {
                            position: std::array::from_fn(|a| {
                                center[a] + s * (dx * right[a] + dy * up[a])
                            }),
                            uv: uv[n],
                            color: (argb & 0xff00ff00)
                                | ((argb >> 16) & 255)
                                | ((argb & 255) << 16),
                        }
                    });
                    let flags = flags | if light & 0xff != 0 { 4 } else { 0 };
                    faces.push(Face {
                        key: FaceKey {
                            owner: particle,
                            part,
                            primitive: light >> 8,
                            texture: tid,
                            flags,
                        },
                        corners,
                        triangle: false,
                        emission: (0, 0.),
                        texture,
                    });
                }
                continue;
            }
            if topology == 2 {
                if stride != 96
                    || flags != 0
                    || texture != WHITE
                    || faces.len() + count as usize > 2_000_000
                {
                    return Err(Error::Invalid("物品面源布局无效"));
                }
                let items = items.ok_or(Error::Invalid("物品面缺少已发布材质世代"))?;
                let state = read::<u32>(&mut bytes)?;
                let pose = read::<[[f32; 4]; 3]>(&mut bytes)?;
                if !pose.iter().flatten().all(|v| v.is_finite()) {
                    return Err(Error::Invalid("物品姿态非有限"));
                }
                let data = take(&mut bytes, count as usize * 96)?;
                faces
                    .try_reserve(count as usize)
                    .map_err(scene_cache::Error::from)?;
                for (i, mut quad) in data.chunks_exact(96).enumerate() {
                    let [texture, sprite, lighting, tint] = read::<[u32; 4]>(&mut quad)?;
                    if texture != WHITE
                        && texture != BLOCK_ATLAS
                        && texture as usize >= textures.len()
                    {
                        return Err(Error::Invalid("物品图集索引无效"));
                    }
                    let layer = lighting & 255;
                    if layer > 2 || lighting >> 8 > 15 {
                        return Err(Error::Invalid("物品源图层或发光等级无效"));
                    }
                    let emission = items
                        .surface(
                            (state != WHITE).then_some(state),
                            sprite,
                            lighting >> 8,
                            layer == 1,
                        )
                        .ok_or(Error::Invalid("物品引用未知方块状态"))?;
                    let flags =
                        layer | 256 | (u32::from(emission.1 > 0.) << 2) | ((lighting >> 8) << 16);
                    let color = (tint & 0xff00ff00) | ((tint >> 16) & 255) | ((tint & 255) << 16);
                    let vertices = read::<[[f32; 5]; 4]>(&mut quad)?;
                    let corners = vertices.map(|[x, y, z, u, v]| Vertex {
                        position: pose.map(|r| r[0] * x + r[1] * y + r[2] * z + r[3]),
                        uv: [u, v],
                        color,
                    });
                    if !corners
                        .iter()
                        .all(|v| v.position.iter().chain(&v.uv).all(|x| x.is_finite()))
                    {
                        return Err(Error::Invalid("物品面位置或 UV 非有限"));
                    }
                    faces.push(Face {
                        key: FaceKey {
                            owner,
                            part,
                            primitive: i as u32,
                            texture: (u64::from(state) << 32) | u64::from(sprite),
                            flags,
                        },
                        corners,
                        triangle: false,
                        texture,
                        emission,
                    });
                }
                continue;
            }
            let triangles = match topology {
                3 if count % 3 == 0 => count / 3,
                4 if count % 4 == 0 => count / 4,
                5 | 6 => count.saturating_sub(2),
                7 if count % 4 == 0 => count / 4,
                _ => return Err(Error::Invalid("动态网格拓扑或顶点数无效")),
            };
            if !(12..=512).contains(&stride)
                || pos > stride - 12
                || (color != WHITE && color > stride - 4)
                || (uv != WHITE && uv > stride - 8)
                || flags & !63 != 0
            {
                return Err(Error::Invalid("动态 span 的顶点布局无效"));
            }
            let data = take(&mut bytes, count as usize * stride as usize)?;
            let capacity = triangles as usize * if topology == 7 { 3 } else { 1 };
            if faces.len() + capacity > 2_000_000 {
                return Err(Error::Invalid("活动动态面数过多"));
            }
            faces
                .try_reserve(capacity)
                .map_err(scene_cache::Error::from)?;
            let vertex = |index: u32| -> Result<Vertex, Error> {
                let src = &data[(index * stride) as usize..];
                let position: [f32; 3] = bytemuck::pod_read_unaligned(&src[pos as usize..][..12]);
                let uv: [f32; 2] = if uv == WHITE {
                    [0.; 2]
                } else {
                    bytemuck::pod_read_unaligned(&src[uv as usize..][..8])
                };
                let color = if color == WHITE {
                    WHITE
                } else {
                    bytemuck::pod_read_unaligned(&src[color as usize..][..4])
                };
                if !position.iter().chain(&uv).all(|n| n.is_finite()) {
                    return Err(Error::Invalid("动态顶点包含非有限值"));
                }
                Ok(Vertex {
                    position,
                    uv,
                    color,
                })
            };
            for n in 0..triangles {
                let key = FaceKey {
                    owner,
                    part,
                    primitive: n,
                    texture: tid,
                    flags,
                };
                if topology == 7 {
                    // 26.3 的 LINES 每个端点连续写两遍。光追渔线使用具有截面的细线。
                    line_faces(&mut faces, key, texture, vertex(4 * n)?, vertex(4 * n + 2)?);
                    continue;
                }
                let indices = match topology {
                    3 => [3 * n, 3 * n + 1, 3 * n + 2, 3 * n + 2],
                    4 => [4 * n, 4 * n + 1, 4 * n + 2, 4 * n + 3],
                    5 if n % 2 == 1 => [n + 1, n, n + 2, n + 2],
                    5 => [n, n + 1, n + 2, n + 2],
                    6 => [0, n + 1, n + 2, n + 2],
                    _ => unreachable!("拓扑已在源边界验证"),
                };
                let [a, b, c, d] = indices;
                let c = vertex(c)?;
                let corners = [
                    vertex(a)?,
                    vertex(b)?,
                    c,
                    if topology == 4 { vertex(d)? } else { c },
                ];
                faces.push(Face {
                    key,
                    corners,
                    triangle: topology != 4,
                    emission: (0, 0.),
                    texture,
                });
            }
        }
        if !bytes.is_empty() {
            return Err(Error::Invalid("动态帧末尾有剩余字节"));
        }
        Ok(Self {
            origin,
            textures,
            faces,
            models,
        })
    }
}

// 原版线宽定义在屏幕空间。渔线需要在反射与阴影中共享同一表面，取直径 1/64 格。
fn line_faces(out: &mut Vec<Face>, key: FaceKey, texture: u32, a: Vertex, b: Vertex) {
    let unit = |v: [f32; 3]| {
        let length = v.iter().map(|n| n * n).sum::<f32>().sqrt();
        (length > 1e-8 && length.is_finite()).then(|| v.map(|n| n / length))
    };
    let cross = |x: [f32; 3], y: [f32; 3]| {
        std::array::from_fn(|i| x[(i + 1) % 3] * y[(i + 2) % 3] - x[(i + 2) % 3] * y[(i + 1) % 3])
    };
    let Some(axis) = unit(std::array::from_fn(|i| b.position[i] - a.position[i])) else {
        return;
    };
    let reference = if axis[1].abs() < 0.9 {
        [0., 1., 0.]
    } else {
        [0., 0., 1.]
    };
    let tangent = unit(cross(axis, reference)).expect("基向量与单位轴不平行");
    let bitangent = cross(axis, tangent);
    let ring = [[1., 0.], [-0.5, 0.8660254], [-0.5, -0.8660254]];
    let offset = |v: Vertex, i: usize| Vertex {
        position: std::array::from_fn(|k| {
            v.position[k] + (tangent[k] * ring[i][0] + bitangent[k] * ring[i][1]) / 128.
        }),
        ..v
    };
    for i in 0..3 {
        let j = (i + 1) % 3;
        out.push(Face {
            key: FaceKey {
                primitive: key.primitive * 3 + i as u32,
                ..key
            },
            texture,
            triangle: false,
            emission: (0, 0.),
            corners: [offset(a, i), offset(b, i), offset(b, j), offset(a, j)],
        });
    }
}

pub(super) struct PriorFace {
    pub uv: [[f32; 2]; 4],
    pub position: [[f32; 3]; 4],
    pub id: u64,
}
pub(super) struct PriorModel {
    pub transform: [[f32; 4]; 3],
    pub first_id: u64,
}
pub(super) struct Prior {
    pub models: HashMap<(FaceKey, u64), PriorModel>,
    pub origin: [f64; 3],
    pub faces: HashMap<(FaceKey, u8), PriorFace>,
}
impl Default for Prior {
    fn default() -> Self {
        Self {
            models: HashMap::new(),
            origin: [0.; 3],
            faces: HashMap::new(),
        }
    }
}
