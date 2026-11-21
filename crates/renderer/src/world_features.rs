//! 原版功能几何。仿射 quad 和一般三角形共享设备期 BLAS；活动帧只发布实例与表面数据。
use super::feature_source::{BLOCK_ATLAS, Prior, PriorFace, Source, Texture, WHITE};
use super::*;
use bytemuck::{Pod, Zeroable};
use std::collections::hash_map::Entry;

const INSTANCE_BIT: u32 = 1 << 23;
const A: [f32; 3] = [0., 0., 0.];
const B: [f32; 3] = [1., 0., 0.];
const C: [f32; 3] = [1., 1., 0.];
const D: [f32; 3] = [0., 1., 0.];
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Surface {
    material: [u32; 4],
    identity: [u32; 4],
    position: [[f32; 4]; 4],
    previous: [[f32; 4]; 4],
    uv: [[f32; 2]; 4],
}
#[repr(C)]
#[derive(Pod, Zeroable, Clone, Copy)]
struct Pose {
    current: [[f32; 4]; 3],
    old: [[f32; 4]; 3],
    mesh: [u32; 4],
    material: [u32; 4],
    identity: [u32; 4],
}
#[derive(Clone, Copy, PartialEq)]
struct Placement {
    shape: usize,
    slot: u32,
    mask: u8,
    matrix: [[f32; 4]; 3],
}
struct TextureCopy {
    source: Texture,
    offset: u32,
    kernel: ComputePipeline,
}
pub(super) struct Features {
    shapes: [vulkan::TriangleBlas; 2],
    pub surfaces: Buffer,
    surface_capacity: usize,
    pub pixels: Buffer,
    pixel_capacity: u32,
    copy: ComputePipeline,
    textures: Vec<TextureCopy>,
    dirty: Vec<usize>,
    pub top: Option<AccelerationStructure>,
    terrain: Option<AccelerationStructure>,
    placements: Vec<Placement>,
    previous: Prior,
    pending: Option<Prior>,
    next_id: u64,
    pub first_material: Option<u32>,
    catalog: Option<feature_models::Catalog>,
    candidate: Option<feature_models::Catalog>,
    pose_program: ComputePipeline,
    pose_kernel: Option<ComputePipeline>,
    pose_buffer: Buffer,
    pose_capacity: usize,
    pose_count: u32,
}
impl Features {
    pub fn new(ctx: &mut Context, atlas: Image, placeholder: Buffer) -> Result<Self, Error> {
        let tri = [[A, B, D]];
        let quad = [[A, B, C], [A, C, D]];
        let batch =
            ctx.triangle_blas_batch(&[tri.as_slice(), quad.as_slice()].map(|triangles| {
                vulkan::TriangleBlasInput {
                    triangles,
                    opaque: false,
                    opacity: None,
                }
            }))?;
        let binds = Bindings::new(
            ctx,
            &[
                Binding::SampledImage(atlas),
                Binding::StorageBuffer(placeholder),
                Binding::StorageBuffer(placeholder),
            ],
        )?;
        let copy = ComputePipeline::new(
            ctx,
            binds,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_texture.spv")),
                c"feature_texture",
            ),
        )?;
        let binds = Bindings::new(
            ctx,
            &std::array::from_fn::<_, 3, _>(|_| Binding::StorageBuffer(placeholder)),
        )?;
        let pose_program = ComputePipeline::new(
            ctx,
            binds,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/feature_models.spv")),
                c"feature_models",
            ),
        )?;
        Ok(Self {
            catalog: None,
            candidate: None,
            pose_program,
            pose_buffer: placeholder,
            pose_kernel: None,
            pose_capacity: 0,
            pose_count: 0,
            shapes: [batch.blases[0], batch.blases[1]],
            surfaces: placeholder,
            surface_capacity: 0,
            pixels: placeholder,
            pixel_capacity: 0,
            copy,
            textures: Vec::new(),
            dirty: Vec::new(),
            top: None,
            terrain: None,
            placements: Vec::new(),
            previous: Prior::default(),
            pending: None,
            next_id: 1,
            first_material: None,
        })
    }
    pub fn mark_materials(&self, keys: &mut [materials::Key]) {
        if self.candidate.is_some() {
            for (_, variant) in keys
                .last_chunk_mut::<3>()
                .expect("候选目录始终附带三种宿主表面材质")
            {
                *variant |= materials::INSTANCE_COLOR;
            }
        }
    }
    pub fn retire_catalog(&mut self) {
        self.catalog = None;
        self.first_material = None;
    }
    pub fn reset(&mut self, placeholder: Buffer) {
        self.pose_buffer = placeholder;
        self.surfaces = placeholder;
        self.pixels = placeholder;
        self.pose_capacity = 0;
        self.surface_capacity = 0;
        self.pixel_capacity = 0;
        self.pose_kernel = None;
        self.pose_count = 0;
        self.top = None;
        self.terrain = None;
        self.placements.clear();
        self.previous = Prior::default();
        self.pending = None;
        self.textures.clear();
        self.dirty.clear();
    }
    fn surface_material(
        &self,
        first: u32,
        flags: u32,
        texture: u32,
        slots: &[usize],
    ) -> ([u32; 4], u32) {
        let (offset, width, height) = match texture {
            WHITE | BLOCK_ATLAS => (texture, 0, 0),
            i => {
                let t = &self.textures[slots[i as usize]];
                (t.offset, t.source.width, t.source.height)
            }
        };
        let variant = if flags & 32 != 0 {
            2
        } else {
            u32::from(flags & 4 != 0)
        };
        ([first + variant, flags, offset, width], height)
    }
    pub fn accepted(&mut self) {
        if let Some(frame) = self.pending.take() {
            self.previous = frame;
        }
        self.dirty.clear();
    }
    pub fn roots(&self, roots: &mut Vec<vulkan::native::Resource>) {
        roots.extend(self.shapes.map(vulkan::native::Resource::from));
        roots.extend([self.surfaces, self.pixels].map(vulkan::native::Resource::from));
        if let Some(c) = &self.catalog {
            c.roots(roots);
        }
        if let Some(c) = &self.candidate {
            c.roots(roots);
        }
        roots.push(self.pose_program.into());
        roots.push(self.pose_buffer.into());
        roots.extend(self.pose_kernel.map(vulkan::native::Resource::from));
        roots.push(self.copy.into());
        roots.extend(self.top.map(vulkan::native::Resource::from));
        roots.extend(
            self.textures
                .iter()
                .map(|t| vulkan::native::Resource::from(t.kernel)),
        );
    }
    pub fn record_textures(&self, ctx: &mut Context) -> Result<Option<Recorded>, Error> {
        if self.dirty.is_empty() && self.pose_count == 0 {
            return Ok(None);
        }
        let mut commands = Commands::new(ctx)?;
        commands.barrier();
        for &index in &self.dirty {
            let tex = &self.textures[index];
            commands.dispatch(
                tex.kernel,
                [
                    tex.source.width.div_ceil(8),
                    tex.source.height.div_ceil(8),
                    1,
                ],
            )?;
        }
        if self.pose_count != 0 {
            commands.dispatch(
                self.pose_kernel.expect("有活动模型时已绑定源顶点与姿态"),
                [
                    self.pose_count.min(65535),
                    self.pose_count.div_ceil(65535),
                    1,
                ],
            )?;
        }
        commands.barrier();
        Ok(Some(commands.finish(ctx)?))
    }
    // 宿主在同队列已结束上帧，在本次渲染完成前保活包内视图。
    unsafe fn prepare_textures(
        &mut self,
        ctx: &mut Context,
        src: &[Texture],
    ) -> Result<(Vec<usize>, bool), Error> {
        let count = src
            .iter()
            .try_fold(0u32, |total, t| total.checked_add(t.width * t.height))
            .ok_or(Error::Invalid("动态纹理容量溢出"))?;
        if count > 128 * 1024 * 1024 {
            return Err(Error::Invalid("活动纹理超过 512 MiB"));
        }
        let grow = count > self.pixel_capacity;
        let pixels = if grow {
            Buffer::new(ctx, u64::from(count) * 4, false)?
        } else {
            self.pixels
        };
        let mut new = Vec::with_capacity(src.len());
        let mut dirty = Vec::new();
        let mut offset = 0;
        let mut order: Vec<_> = (0..src.len()).collect();
        order.sort_unstable_by_key(|&i| (src[i].identity, src[i].view));
        let mut slots = vec![0; src.len()];
        for (i, source) in order.into_iter().enumerate() {
            slots[source] = i;
            let t = &src[source];
            let old = self.textures.get(i).filter(|v| {
                !grow
                    && v.offset == offset
                    && v.source.view == t.view
                    && v.source.identity == t.identity
                    && v.source.width == t.width
                    && v.source.height == t.height
                    && v.source.srgb == t.srgb
            });
            let kernel = if let Some(v) = old {
                v.kernel
            } else {
                // SAFETY: 调用者保证本设备的 GENERAL sampled view 在宿主帧完成前有效。
                let view =
                    unsafe { ExternalBinding::new(ctx, t.view, ExternalKind::SampledImage)? };
                let params = Buffer::upload(
                    ctx,
                    bytemuck::cast_slice(&[offset, t.width, t.height, t.srgb]),
                )?;
                let binds = Bindings::new(
                    ctx,
                    &[
                        Binding::External(view),
                        Binding::StorageBuffer(pixels),
                        Binding::StorageBuffer(params),
                    ],
                )?;
                self.copy.rebind(ctx, binds)?
            };
            if old.is_none_or(|v| v.source.revision != t.revision)
                || self.dirty.binary_search(&i).is_ok()
            {
                dirty.push(i);
            }
            new.push(TextureCopy {
                source: *t,
                offset,
                kernel,
            });
            offset += t.width * t.height;
        }
        let rebound = new.len() != self.textures.len()
            || new
                .iter()
                .zip(&self.textures)
                .any(|(a, b)| a.kernel != b.kernel);
        self.textures = new;
        self.dirty = dirty;
        self.pixels = pixels;
        self.pixel_capacity = self.pixel_capacity.max(count);
        Ok((slots, rebound))
    }
}
fn relative(p: [f32; 3], origin: [f64; 3], anchor: [f64; 3]) -> [f32; 3] {
    std::array::from_fn(|i| ((origin[i] - anchor[i]) + f64::from(p[i])) as f32)
}
const fn feature_mask(flags: u32) -> u8 {
    let visibility = if flags & 64 != 0 { 4 } else { 6 };
    visibility | if flags & 8 == 0 { 1 } else { 0 }
}
fn matrix(p: [[f32; 3]; 4], tri: bool) -> Option<[[f32; 4]; 3]> {
    let x: [f32; 3] = std::array::from_fn(|i| p[1][i] - p[0][i]);
    let y: [f32; 3] = std::array::from_fn(|i| p[if tri { 2 } else { 3 }][i] - p[0][i]);
    let z: [f32; 3] =
        std::array::from_fn(|i| x[(i + 1) % 3] * y[(i + 2) % 3] - x[(i + 2) % 3] * y[(i + 1) % 3]);
    let len = z.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !len.is_finite() || len < 1e-14 {
        return None;
    }
    Some(std::array::from_fn(|i| [x[i], y[i], z[i] / len, p[0][i]]))
}
impl Gpu {
    /// 准备包含原版功能表面的材质世代。源材质索引和地形灯的注册身份保持稳定。
    ///
    /// PFM1 保存不可变的组件顶点与 UV，返回前完成本世代全部组件 BLAS 的准备。
    /// 两份源数据都只在调用期间借用。
    /// # Errors
    /// 源格式、材质、分配、程序编译或设备构建失败时返回对应错误。
    pub fn replace_world_resources(
        &mut self,
        source: ResourceUpload<'_>,
        models: &[u8],
    ) -> Result<(), Error> {
        let first =
            u32::try_from(source.materials.len()).map_err(|_| Error::Invalid("材质过多"))?;
        let mut defs = source.materials.to_vec();
        let surface = MaterialDefinition::default();
        let mut luminous = surface;
        luminous.emission = [1., 1., 1., 1.];
        luminous.control[1] = 3;
        let mut beam = luminous;
        beam.surface = [1., 0., 0., 0.];
        beam.extinction = [1., 0., 0., 0.];
        beam.control[0] = 3;
        beam.base_roughness = [1., 1., 1., 0.];
        defs.extend([surface, luminous, beam]);
        let mut areas = source.light_areas.to_vec();
        areas.extend([[u32::MAX, 0, 0, 0]; 3]);
        self.complete_previous()?;
        let catalog = match feature_models::Catalog::prepare(
            &mut self.context,
            models,
            self.geometry_placeholder,
        ) {
            Ok(catalog) => catalog,
            Err(error) => {
                self.collect_resources()?;
                return Err(error);
            }
        };
        self.features.candidate = Some(catalog);
        if let Err(e) = self.replace_resources(ResourceUpload {
            materials: &defs,
            light_areas: &areas,
            ..source
        }) {
            self.features.candidate = None;
            self.collect_resources()?;
            return Err(e);
        }
        self.features.catalog = self.features.candidate.take();
        self.features.first_material = Some(first);
        Ok(())
    }

    /// 从同次提取的 PTF1 源包发布动态实例；旧帧由渲染完成回调推进，出生的面拒绝历史。
    /// # Safety
    /// 包中视图来自本逻辑设备，处于 GENERAL，宿主保活至本帧结束，且已等待上帧。
    /// # Errors
    /// 拒绝无效源、未准备世代，以及容量、设备和 TLAS 提交失败。
    pub unsafe fn set_world_features(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.complete_previous()?;
        let source = Source::decode(bytes)?;
        if source.faces.is_empty()
            && source.models.is_empty()
            && self.features.top.is_none()
            && self.features.textures.is_empty()
        {
            self.features.previous = Prior::default();
            self.features.pending = None;
            return Ok(());
        }
        let material = self
            .features
            .first_material
            .ok_or(Error::Invalid("尚未准备动态材质"))?;
        let old_buffers = (self.features.surfaces, self.features.pixels);
        // SAFETY: 继承本入口的视图与外部队列契约，描述符只在本帧读取。
        let (slots, textures_rebound) = unsafe {
            self.features
                .prepare_textures(&mut self.context, &source.textures)?
        };
        let mut surfaces = Vec::with_capacity(source.faces.len());
        let mut placements = Vec::with_capacity(source.faces.len() + source.models.len());
        let mut pending = Prior {
            origin: source.origin,
            ..Default::default()
        };
        pending
            .faces
            .try_reserve(source.faces.len())
            .map_err(scene_cache::Error::from)?;
        pending
            .models
            .try_reserve(source.models.len())
            .map_err(scene_cache::Error::from)?;
        for face in &source.faces {
            let p = face.corners.map(|v| v.position);
            // 非仿射 quad 保留原版对角线，分别用两个三角形实例表达。
            let affine = (0..3).all(|i| {
                (p[0][i] - p[1][i] + p[2][i] - p[3][i]).abs()
                    <= 8. * f32::EPSILON * (1. + p.iter().map(|v| v[i].abs()).fold(0., f32::max))
            });
            let split = !face.triangle && !affine;
            for side in 0..=usize::from(split) {
                let inds = if side == 1 {
                    [0, 2, 3, 3]
                } else if split {
                    [0, 1, 2, 2]
                } else {
                    [0, 1, 2, 3]
                };
                let vertices = inds.map(|i| face.corners[i]);
                let local = vertices.map(|v| v.position);
                let current = local.map(|p| relative(p, source.origin, self.anchor));
                let tri = face.triangle || split;
                let Some(mut transform) = matrix(local, tri) else {
                    continue;
                };
                for i in 0..3 {
                    transform[i][3] = current[0][i];
                }
                let key = (face.key, side as u8);
                let uv = vertices.map(|v| v.uv);
                let prior = self
                    .features
                    .previous
                    .faces
                    .get(&key)
                    .filter(|p| face.key.owner != 0 && p.uv == uv);
                let id = if let Some(prior) = prior {
                    prior.id
                } else {
                    let id = self.features.next_id;
                    self.features.next_id =
                        id.checked_add(1).ok_or(Error::Invalid("动态面身份耗尽"))?;
                    id
                };
                if face.key.owner != 0 {
                    match pending.faces.entry(key) {
                        Entry::Vacant(e) => {
                            e.insert(PriorFace {
                                position: local,
                                uv,
                                id,
                            });
                        }
                        Entry::Occupied(_) => {
                            return Err(Error::Invalid("同一动态源重复声明面身份"));
                        }
                    }
                }
                let old = prior.map_or(current, |v| {
                    v.position
                        .map(|p| relative(p, self.features.previous.origin, self.anchor))
                });
                let flags = face.key.flags;
                let (surface_material, height) =
                    self.features
                        .surface_material(material, flags, face.texture, &slots);
                placements.push(Placement {
                    shape: usize::from(!tri),
                    slot: surfaces.len() as u32,
                    mask: feature_mask(flags),
                    matrix: transform,
                });
                surfaces.push(Surface {
                    material: surface_material,
                    identity: [
                        id as u32,
                        (id >> 32) as u32 | 0x80000000,
                        height,
                        u32::from(prior.is_some()),
                    ],
                    position: std::array::from_fn(|i| {
                        [
                            current[i][0],
                            current[i][1],
                            current[i][2],
                            f32::from_bits(vertices[i].color),
                        ]
                    }),
                    previous: old.map(|p| [p[0], p[1], p[2], 0.]),
                    uv,
                });
            }
        }
        let mut next_slot = surfaces.len() as u32;
        let catalog = self
            .features
            .catalog
            .as_ref()
            .expect("已发布世代持有组件目录");
        let mut poses = Vec::with_capacity(source.models.len() + 1);
        poses.push(Pose::zeroed());
        for model in &source.models {
            let &(first, count, _) = catalog
                .mesh
                .get(model.index as usize)
                .ok_or(Error::Invalid("实例引用未知模型组件"))?;
            let flags = model.key.flags;
            let mut current = model.transform;
            let pos = relative(current.map(|r| r[3]), source.origin, self.anchor);
            for i in 0..3 {
                current[i][3] = pos[i];
            }
            // 原版缩放为零时组件不可见，既不进入 TLAS 也不建立历史。
            if !vulkan::instance_transform_supported(&current) {
                continue;
            }
            let previous = self
                .features
                .previous
                .models
                .get(&(model.key, model.bone))
                .filter(|_| model.key.owner != 0);
            let id = if let Some(p) = previous {
                p.first_id
            } else {
                let id = self.features.next_id;
                self.features.next_id = id
                    .checked_add(u64::from(count))
                    .ok_or(Error::Invalid("模型面身份耗尽"))?;
                id
            };
            let mut old = previous.map_or(current, |p| p.transform);
            if previous.is_some() {
                let pos = relative(
                    old.map(|r| r[3]),
                    self.features.previous.origin,
                    self.anchor,
                );
                for i in 0..3 {
                    old[i][3] = pos[i];
                }
            }
            if model.key.owner != 0
                && pending
                    .models
                    .insert(
                        (model.key, model.bone),
                        feature_source::PriorModel {
                            transform: model.transform,
                            first_id: id,
                        },
                    )
                    .is_some()
            {
                return Err(Error::Invalid("模型部件身份重复"));
            }
            let (surface_material, height) =
                self.features
                    .surface_material(material, flags, model.texture, &slots);
            poses.push(Pose {
                current,
                old,
                mesh: [first, count, next_slot, model.color],
                material: surface_material,
                identity: [
                    id as u32,
                    (id >> 32) as u32 | 0x80000000,
                    height,
                    u32::from(previous.is_some()),
                ],
            });
            placements.push(Placement {
                shape: 2 + model.index as usize,
                slot: next_slot,
                mask: feature_mask(flags),
                matrix: current,
            });
            next_slot = next_slot
                .checked_add(count)
                .filter(|n| *n < INSTANCE_BIT)
                .ok_or(Error::Invalid("动态面槽超出实例域"))?;
        }
        let changed = placements != self.features.placements
            || (!placements.is_empty() && self.features.terrain != Some(self.scene.top));
        let top = if placements.is_empty() {
            None
        } else if !changed {
            self.features.top
        } else {
            let regions = self.region_cache.entries();
            if regions.len() >= INSTANCE_BIT as usize {
                return Err(Error::Invalid("区域索引超过 23 位"));
            }
            let mut items = Vec::with_capacity(regions.len() + placements.len());
            let mut masks = Vec::with_capacity(items.capacity());
            for (index, r) in regions.iter().enumerate() {
                let offset: [f32; 3] =
                    std::array::from_fn(|i| (f64::from(r.key[i]) * 64. - self.anchor[i]) as f32);
                items.push(TriangleInstance {
                    geometry: vulkan::BatchBlas::Existing(r.blas.expect("已发布区域持有 BLAS")),
                    custom_index: index as u32,
                    transform: [
                        [1., 0., 0., offset[0]],
                        [0., 1., 0., offset[1]],
                        [0., 0., 1., offset[2]],
                    ],
                });
                masks.push(0xff);
            }
            for v in &placements {
                items.push(TriangleInstance {
                    geometry: vulkan::BatchBlas::Existing(if v.shape < 2 {
                        self.features.shapes[v.shape]
                    } else {
                        catalog.mesh[v.shape - 2].2
                    }),
                    custom_index: INSTANCE_BIT | v.slot,
                    transform: v.matrix,
                });
                masks.push(v.mask);
            }
            Some(self.context.enqueue_masked_instances(&items, &masks)?)
        };
        let len = next_slot as usize;
        if len > self.features.surface_capacity {
            let capacity = len.next_power_of_two();
            self.features.pose_kernel = None;
            self.features.surfaces = Buffer::mapped_device_local(
                &mut self.context,
                (capacity * size_of::<Surface>()) as u64,
            )?;
            self.features.surface_capacity = capacity;
        }
        let old_pose = self.features.pose_buffer;
        let pose_count = (poses.len() - 1) as u32;
        if pose_count != 0 {
            let grow = poses.len() > self.features.pose_capacity;
            if grow {
                let capacity = poses.len().next_power_of_two();
                self.features.pose_buffer = Buffer::mapped_device_local(
                    &mut self.context,
                    (capacity * size_of::<Pose>()) as u64,
                )?;
                self.features.pose_capacity = capacity;
            }
            if grow
                || self.features.pose_kernel.is_none()
                || self.features.surfaces != old_buffers.0
            {
                let binds = Bindings::new(
                    &mut self.context,
                    &[
                        catalog.vertices,
                        self.features.pose_buffer,
                        self.features.surfaces,
                    ]
                    .map(Binding::StorageBuffer),
                )?;
                self.features.pose_kernel = Some(
                    self.features
                        .pose_program
                        .rebind(&mut self.context, binds)?,
                );
            }
            poses[0].mesh[0] = pose_count;
            self.features
                .pose_buffer
                .write(&mut self.context, 0, bytemuck::cast_slice(&poses))?;
        }
        let pose_storage_changed = self.features.pose_buffer != old_pose;
        self.features.pose_count = pose_count;
        if !surfaces.is_empty() {
            self.features
                .surfaces
                .write(&mut self.context, 0, bytemuck::cast_slice(&surfaces))?;
        }
        self.features.top = top;
        self.features.terrain = Some(self.scene.top);
        self.features.placements = placements;
        self.features.pending = Some(pending);
        let rebind = changed || old_buffers != (self.features.surfaces, self.features.pixels);
        if rebind {
            self.renderer = None;
            self.guide_pipeline = None;
            self.transition_history(history::Event::Accumulation(
                history::AccumulationEvent::SceneChanged,
            ))?;
        }
        if rebind || textures_rebound || pose_storage_changed {
            self.collect_resources()?;
        }
        Ok(())
    }
}
