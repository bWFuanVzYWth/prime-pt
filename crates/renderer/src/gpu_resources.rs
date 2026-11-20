//! 资源世代的完整准备与发布；旧世代按实际GPU完成边界退役。

use super::*;

impl Gpu {
    /// 完成旧提交后选择已准备的照明与导引程序；保留数据和 PT/RR 历史。
    /// 不支持时保持普通程序；仅选择资源边界已准备的变体，不创建pipeline或SBT。
    pub fn set_ser(&mut self, requested: bool) -> Result<(), Error> {
        if (requested && self.context.capabilities().invocation_reorder) == self.invocation_reorder
        {
            return Ok(());
        }
        self.complete_previous()?;
        let result = self.set_ser_inner(requested);
        self.collect_resources()?;
        result
    }

    pub(super) fn set_ser_inner(&mut self, requested: bool) -> Result<(), Error> {
        let enabled = requested && self.context.capabilities().invocation_reorder;
        if enabled == self.invocation_reorder {
            return Ok(());
        }
        if enabled
            && self.generation.is_some()
            && self
                .generation
                .as_ref()
                .and_then(|g| g.reordered.as_ref())
                .is_none()
        {
            return Err(Error::Invalid("资源准备未提供SER程序"));
        }
        self.invocation_reorder = enabled;
        self.renderer = None;
        self.guide_pipeline = None;

        Ok(())
    }

    /// 完成旧命令后原子替换atlas与其特化程序；失败保留上一代。
    #[cfg(test)]
    pub(super) fn set_atlas(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), Error> {
        self.complete_previous()?;
        let result = self.set_atlas_inner(width, height, pixels);
        self.collect_resources()?;
        result
    }

    #[cfg(test)]
    pub(super) fn set_atlas_inner(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), Error> {
        let expected = (u64::from(width) * u64::from(height))
            .checked_mul(4)
            .ok_or(Error::Invalid("atlas 字节数溢出"))?;
        if width == 0 || height == 0 || expected != pixels.len() as u64 {
            return Err(Error::Invalid("atlas 尺寸与 RGBA8 字节数不匹配"));
        }
        let definitions = materials::defaults();
        let prepared = materials::bindings(
            self.resource_generation,
            &definitions,
            [width, height],
            pixels,
            &[],
            &[],
        )?;
        let atlas = Image::new(
            &mut self.context,
            [width, height, 1],
            ImageFormat::Rgba8Srgb,
        )?;
        atlas.upload(&mut self.context, pixels)?;
        let (resources, generation) = self.prepare_material_generation(
            atlas,
            self.material_gpu,
            &prepared.keys,
            &definitions,
            self.opacity.map.is_some(),
        )?;
        self.invalidate_history(GlobalReset::SceneDomainReplaced)?;
        self.atlas = atlas;
        self.material_gpu = resources;
        self.material_coverage = definitions.iter().map(materials::tests_coverage).collect();
        self.generation = Some(generation);
        self.renderer = None;
        self.guide_pipeline = None;

        Ok(())
    }

    /// 完整资源世代的唯一准备与发布入口；不得由section、实例或逐帧路径调用。
    ///
    /// 全部图像、静态alias、材质范围、程序和可用SER变体成功后一次发布。
    /// 旧命令须已完成；调用后在绘制前提交同一资源世代的region视图。
    pub fn replace_resources(&mut self, source: ResourceUpload<'_>) -> Result<(), Error> {
        self.cancel_packets()?;
        self.complete_previous()?;
        let result = (|| {
            let bytes = u64::from(source.extent[0])
                .checked_mul(u64::from(source.extent[1]))
                .and_then(|n| n.checked_mul(4))
                .ok_or(Error::Invalid("资源atlas尺寸溢出"))?;
            if source.generation <= self.resource_generation
                || source.extent.iter().any(|&v| v == 0 || v > 16384)
                || [
                    source.albedo.len(),
                    source.mers.len(),
                    source.auxiliary.len(),
                ]
                .into_iter()
                .any(|n| n as u64 != bytes)
                || source.materials.is_empty()
                || source.materials.len() != source.light_areas.len()
            {
                return Err(Error::Invalid("完整资源图像或材质域无效"));
            }
            for area in source.light_areas {
                if area[0] != u32::MAX
                    && (area[0] > light_sampling::SELECTORS
                        || area[1..3]
                            .iter()
                            .any(|&v| !f32::from_bits(v).is_finite() || f32::from_bits(v) < 0.))
                {
                    return Err(Error::Invalid("静态quad面积分布无效"));
                }
            }
            let region_cache = scene_cache::Cache::new()?;
            let lighting = lighting::Lighting::new()?;
            let prepared = materials::bindings(
                source.generation,
                source.materials,
                source.extent,
                source.albedo,
                source.mers,
                source.auxiliary,
            )?;
            let definitions = prepared.definitions.as_ref();
            let keys = &prepared.keys;
            validate_aliases(definitions, source.emission_alias_words)?;
            let mut light_sides = Vec::new();
            light_sides
                .try_reserve_exact(source.materials.len())
                .map_err(scene_cache::Error::from)?;
            light_sides.extend(definitions.iter().zip(keys).map(|(m, (flow, _))| {
                if flow.emits() {
                    materials::emission_sides(m)
                } else {
                    0
                }
            }));
            let opacity = opacity::Prepared::new(&mut self.context, &source, definitions)?;
            let has_emission = light_sides.iter().any(|&side| side != 0);
            let light_weights = materials::emission_weights(definitions)?;
            let mut material_coverage = Vec::new();
            material_coverage
                .try_reserve_exact(definitions.len())
                .map_err(scene_cache::Error::from)?;
            material_coverage.extend(definitions.iter().map(materials::tests_coverage));
            let mut light_areas = Vec::new();
            light_areas
                .try_reserve_exact(source.light_areas.len())
                .map_err(scene_cache::Error::from)?;
            light_areas.extend_from_slice(source.light_areas);
            let extent = [source.extent[0], source.extent[1], 1];
            let atlas = Image::new(&mut self.context, extent, ImageFormat::Rgba8Srgb)?;
            atlas.upload(&mut self.context, source.albedo)?;
            let mers = Image::new(&mut self.context, extent, ImageFormat::Rgba8Unorm)?;
            mers.upload(&mut self.context, source.mers)?;
            let auxiliary = Image::new(&mut self.context, extent, ImageFormat::Rgba8Unorm)?;
            auxiliary.upload(&mut self.context, source.auxiliary)?;
            let alias_bytes = bytemuck::cast_slice(source.emission_alias_words);
            let resources = MaterialGpu {
                table: self.material_gpu.table,
                mers,
                auxiliary,
                emission_alias: Buffer::upload(
                    &mut self.context,
                    if alias_bytes.is_empty() {
                        &[0; 16]
                    } else {
                        alias_bytes
                    },
                )?,
            };
            let (resources, generation) = self.prepare_material_generation(
                atlas,
                resources,
                keys,
                definitions,
                opacity.map.is_some(),
            )?;
            let candidates: Vec<_> = generation
                .roots()
                .chain(opacity.map.map(vulkan::native::Resource::from))
                .collect();
            self.collect_with(&candidates)?;
            // 此后只移动已经准备的所有者；不得以清理失败向宿主报告“保留旧世代”。
            self.renderer = None;
            self.guide_pipeline = None;
            self.transition_history(history::Event::Global(GlobalReset::SceneDomainReplaced))?;
            self.atlas = atlas;
            self.material_gpu = resources;
            self.material_coverage = material_coverage;
            self.opacity = opacity;
            self.light_sides = light_sides;
            self.light_weights = light_weights;
            self.has_emission = has_emission;
            self.region_cache = region_cache;
            self.terrain_uploads.clear();
            self.pending_storage = None;
            self.attribute_revisions.clear();
            self.scene.top = self.program_scene.top;
            self.scene.instance_quad_bases = self.program_scene.instance_quad_bases;
            self.lighting = lighting;
            self.lighting_gpu = self.program_lighting;
            self.light_areas = light_areas;
            self.resource_generation = source.generation;
            self.generation = Some(generation);

            self.lighting_dirty = true;
            Ok(())
        })();
        if result.is_err() {
            self.collect_resources()?;
        } else {
            self.context.flush_pipeline_cache();
        }
        result
    }

    pub(super) fn prepare_material_generation(
        &mut self,
        atlas: Image,
        mut resources: MaterialGpu,
        keys: &[materials::Key],
        definitions: &[MaterialDefinition],
        opacity_enabled: bool,
    ) -> Result<(MaterialGpu, MaterialGeneration), Error> {
        if keys.len() != definitions.len() {
            return Err(Error::Invalid("材质Flow与输入定义数量不匹配"));
        }
        #[cfg(test)]
        let dispatch = self.experiment_dispatch;
        #[cfg(not(test))]
        let dispatch = if self.context.capabilities().invocation_reorder {
            8
        } else {
            0
        };
        let prepare_started = std::time::Instant::now();
        let workers = match std::env::var("PRIME_PT_SHADER_JOBS") {
            Ok(value) => value
                .parse::<std::num::NonZeroUsize>()
                .map_err(|_| Error::Invalid("PRIME_PT_SHADER_JOBS须为正整数"))?,
            Err(std::env::VarError::NotPresent) => std::num::NonZeroUsize::new(
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .min(8),
            )
            .unwrap(),
            Err(_) => return Err(Error::Invalid("PRIME_PT_SHADER_JOBS须为正整数")),
        };
        // 空 OMM 世代无需 RayQuery 的 micromap 模式；有表时能力已由构建边界建立。
        let omm = if opacity_enabled {
            "-DPRIMEPT_OMM=1"
        } else {
            "-DPRIMEPT_OMM=0"
        };
        let mut batch = self.compiler.batch();
        let hit = materials::hit_features(definitions);
        let coverage = format!("-DPRIMEPT_COVERAGE_MASK={}", hit.coverage);
        let emitter_capabilities = materials::emitter_capabilities(definitions, keys);
        let emitters = format!("-DPRIMEPT_EMITTER_CAPABILITIES={emitter_capabilities}");
        let lights = if keys.iter().any(|(flow, _)| flow.emits()) {
            "-DPRIMEPT_LIGHT_MASK=3"
        } else {
            "-DPRIMEPT_LIGHT_MASK=1"
        };
        let hit_source = self.shader_source.with_file_name("pt_hit.slang");
        let program_source = if dispatch == 8 {
            &hit_source
        } else {
            &self.shader_source
        };
        let compile_programs = if dispatch == 8 {
            openpbr::program::ProgramSet::queue_hit_variants
        } else {
            openpbr::program::ProgramSet::queue_variants
        };
        let programs = compile_programs(
            &mut batch,
            keys.iter().copied(),
            openpbr::program::Template {
                source: program_source
                    .to_str()
                    .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                entry: if dispatch == 8 {
                    "shade_hit"
                } else {
                    "shade_opaque"
                },
                output: "flow.spv",
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &[
                    "-fp-mode",
                    "precise",
                    "-DOPENPBR_RECIPE_VARIANT=1",
                    lights,
                    &coverage,
                    &emitters,
                    omm,
                    "-capability",
                    "spvRayTracingPositionFetchKHR",
                ],
            },
        )?;
        let hit_args = [
            "-fp-mode",
            "precise",
            "-DOPENPBR_RECIPE_VARIANT=1",
            lights,
            &coverage,
            &emitters,
            omm,
        ];
        let primary_source = self.shader_source.with_file_name("primary_material.slang");
        let primary_materials = (dispatch == 8)
            .then(|| {
                openpbr::program::ProgramSet::queue_variants(
                    &mut batch,
                    keys.iter().copied(),
                    openpbr::program::Template {
                        source: primary_source
                            .to_str()
                            .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                        entry: "shade_primary",
                        output: "primary-material.spv",
                        target: shader_build::Target::Spirv("spirv_1_4"),
                        args: &hit_args,
                    },
                )
                .map_err(Error::from)
            })
            .transpose()?;
        let material_hits = if (2..=7).contains(&dispatch) {
            Some(openpbr::program::ProgramSet::queue_hit_variants(
                &mut batch,
                programs.plan().keys().iter().copied(),
                openpbr::program::Template {
                    source: self
                        .shader_source
                        .to_str()
                        .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                    entry: "shade_hit",
                    output: "flow-hit.spv",
                    target: shader_build::Target::Spirv("spirv_1_4"),
                    args: &hit_args,
                },
            )?)
        } else {
            None
        };
        let compile_stage = |batch: &mut shader_build::RuntimeBatch<'_>,
                             source: &Path,
                             entry,
                             stage,
                             output,
                             args: &[&str]| {
            let args: Vec<_> = args.iter().copied().chain([omm]).collect();
            batch.queue(&shader_build::Recipe {
                source: source.to_str().ok_or(shader_build::Error::Configuration(
                    "shader路径不是UTF-8".into(),
                ))?,
                entry,
                stage,
                output,
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &args,
            })
        };
        let dispatch_arg = format!("-DPRIMEPT_DISPATCH={dispatch}");
        // raygen不执行材质叶子；显式占位只用于同module中未选中的入口解析。
        let no_material = ["-DOPENPBR_FLOW=0", "-DOPENPBR_VARIANT=0xffffffffu"];
        let args = [
            "-fp-mode",
            "precise",
            "-capability",
            "spvShaderInvocationReorderEXT",
            "-capability",
            "spvRayTracingPositionFetchKHR",
            &dispatch_arg,
            &coverage,
            &emitters,
            no_material[0],
            no_material[1],
        ];
        let native_args: Vec<_> = args
            .iter()
            .copied()
            .chain((dispatch == 8).then_some("-DPRIMEPT_RECONSTRUCTION=0"))
            .chain(cfg!(test).then_some("-DPRIMEPT_REFERENCE_FILM=1"))
            .collect();
        let guided_args: Vec<_> = args
            .iter()
            .copied()
            .chain(["-DPRIMEPT_SEPARATE_GUIDES=1"])
            .collect();
        let ser_args: Vec<_> = native_args
            .iter()
            .copied()
            .chain(["-DPRIMEPT_SER=1"])
            .collect();
        let guided_ser_args: Vec<_> = guided_args
            .iter()
            .copied()
            .chain(["-DPRIMEPT_SER=1"])
            .collect();
        let fused = (dispatch != 0)
            .then(|| {
                compile_stage(
                    &mut batch,
                    program_source,
                    if dispatch == 8 {
                        "render_offline"
                    } else {
                        "render_fused"
                    },
                    "raygeneration",
                    "render-fused.spv",
                    &native_args,
                )
            })
            .transpose()?;
        let primary = if dispatch == 8 {
            ["primary_random", "primary_jittered"]
                .into_iter()
                .map(|entry| {
                    compile_stage(
                        &mut batch,
                        program_source,
                        entry,
                        "raygeneration",
                        "primary.spv",
                        &native_args,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let guide_rg = self.shader_source.with_file_name("guides.slang");
        let guided = (dispatch == 8)
            .then(|| {
                compile_stage(
                    &mut batch,
                    &guide_rg,
                    "guide_main",
                    "raygeneration",
                    "guides.spv",
                    &guided_args,
                )
            })
            .transpose()?;
        let guided_ser = (dispatch == 8 && self.context.capabilities().invocation_reorder)
            .then(|| {
                compile_stage(
                    &mut batch,
                    &guide_rg,
                    "guide_main",
                    "raygeneration",
                    "guides.spv",
                    &guided_ser_args,
                )
            })
            .transpose()?;
        let fused_ser = (dispatch != 0 && self.context.capabilities().invocation_reorder)
            .then(|| {
                compile_stage(
                    &mut batch,
                    program_source,
                    if dispatch == 8 {
                        "render_offline"
                    } else {
                        "render_fused"
                    },
                    "raygeneration",
                    "render-fused.spv",
                    &ser_args,
                )
            })
            .transpose()?;
        let guide_source = self.shader_source.with_file_name("guide_material.slang");
        let mut realtime_graphs = Vec::new();
        let mut reference_graphs = Vec::new();
        if dispatch == 8 {
            for (reordered, args) in [(false, &native_args), (true, &ser_args)] {
                if reordered && !self.context.capabilities().invocation_reorder {
                    continue;
                }
                for entry in ["render_rt2_lane0", "render_rt2_lane1"]
                    .into_iter()
                    .chain(cfg!(test).then_some("render_rt2_merged"))
                {
                    realtime_graphs.push(compile_stage(
                        &mut batch,
                        program_source,
                        entry,
                        "raygeneration",
                        "render-realtime.spv",
                        args,
                    )?);
                }
                if cfg!(test) {
                    reference_graphs.push(compile_stage(
                        &mut batch,
                        program_source,
                        "render_diagnostic",
                        "raygeneration",
                        "render-reference.spv",
                        args,
                    )?);
                }
            }
        }
        let guide_hits = openpbr::program::ProgramSet::queue_hit_variants(
            &mut batch,
            programs
                .plan()
                .keys()
                .iter()
                .copied()
                .map(materials::scattering_key),
            openpbr::program::Template {
                source: guide_source
                    .to_str()
                    .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                entry: "guide_hit",
                output: "guide-hit.spv",
                target: shader_build::Target::Spirv("spirv_1_4"),
                args: &[
                    "-fp-mode",
                    "precise",
                    "-DOPENPBR_RECIPE_VARIANT=1",
                    &coverage,
                    "-capability",
                    "spvRayTracingPositionFetchKHR",
                ],
            },
        )?;
        let primary_guides = (dispatch == 8)
            .then(|| {
                openpbr::program::ProgramSet::queue_variants(
                    &mut batch,
                    programs
                        .plan()
                        .keys()
                        .iter()
                        .copied()
                        .map(materials::scattering_key),
                    openpbr::program::Template {
                        source: guide_source
                            .to_str()
                            .ok_or(Error::Invalid("shader路径不是UTF-8"))?,
                        entry: "guide_primary",
                        output: "primary-guide.spv",
                        target: shader_build::Target::Spirv("spirv_1_4"),
                        args: &hit_args,
                    },
                )
                .map_err(Error::from)
            })
            .transpose()?;
        eprintln!(
            "[prime_pt] shader_prepare begin tasks={} workers={}",
            batch.task_count(),
            workers
        );
        let mut compiled = batch.compile(workers)?;
        let compile_statistics = compiled.statistics();
        let programs = programs.finish(&mut compiled)?;
        let material_hits = material_hits
            .map(|set| set.finish(&mut compiled))
            .transpose()?;

        let guide_hits = guide_hits.finish(&mut compiled)?;
        let primary_materials = primary_materials
            .map(|set| set.finish(&mut compiled))
            .transpose()?;
        let primary_guides = primary_guides
            .map(|set| set.finish(&mut compiled))
            .transpose()?;
        let mut take = |ticket| compiled.take(ticket);
        let primary = primary
            .into_iter()
            .map(&mut take)
            .collect::<Result<Vec<_>, _>>()?;
        let fused = fused.map(&mut take).transpose()?;
        let fused_ser = fused_ser.map(&mut take).transpose()?;
        let realtime_graphs: Vec<_> = realtime_graphs
            .into_iter()
            .map(&mut take)
            .collect::<Result<_, _>>()?;
        let reference_graphs: Vec<_> = reference_graphs
            .into_iter()
            .map(&mut take)
            .collect::<Result<_, _>>()?;
        let guided = guided.map(&mut take).transpose()?;
        let guided_ser = guided_ser.map(&mut take).transpose()?;
        let retained_main_bytes: usize = programs
            .artifacts()
            .iter()
            .map(|artifact| artifact.bytes().len())
            .sum();
        let batch_main_bytes: usize = programs
            .artifacts()
            .iter()
            .chain(material_hits.iter().flat_map(|set| set.artifacts()))
            .chain(guide_hits.artifacts())
            .chain(primary_materials.iter().flat_map(|set| set.artifacts()))
            .chain(primary_guides.iter().flat_map(|set| set.artifacts()))
            .chain(primary.iter())
            .chain(fused.iter())
            .chain(fused_ser.iter())
            .chain(realtime_graphs.iter())
            .chain(reference_graphs.iter())
            .chain(guided.iter())
            .chain(guided_ser.iter())
            .map(|artifact| artifact.bytes().len())
            .sum();

        let mut hit_shaders: Vec<_> = if dispatch == 8 {
            programs.artifacts()
        } else {
            material_hits
                .as_ref()
                .map_or(&[][..], |set| set.artifacts())
        }
        .iter()
        .map(|a| RayShader {
            spirv: a.bytes(),
            entry: c"shade_hit",
        })
        .collect();
        if dispatch != 8 {
            hit_shaders.extend(guide_hits.artifacts().iter().map(|artifact| RayShader {
                spirv: artifact.bytes(),
                entry: c"guide_hit",
            }));
        }
        let table_words = definitions
            .len()
            .checked_mul(28)
            .ok_or(Error::Invalid("材质表尺寸溢出"))?;
        let mut table = Vec::<u32>::new();
        table
            .try_reserve_exact(table_words)
            .map_err(scene_cache::Error::from)?;
        for ((definition, &slot), (flow, _)) in
            definitions.iter().zip(programs.plan().bindings()).zip(keys)
        {
            let guide = guide_hits.plan().bindings()[slot as usize];
            if slot > u32::MAX >> 2 || guide > u32::MAX >> 2 {
                return Err(Error::Invalid("材质程序索引超过30bit"));
            }
            let mut words = definition.words();
            words[4][0] |= slot << 2;
            words[4][1] |= guide << 2;
            words[4][2] |= flow.bits() << 20;
            table.extend(words.into_iter().flatten());
        }
        resources.table = Buffer::upload(&mut self.context, bytemuck::cast_slice(&table))?;
        let descriptors = self.renderer_bindings(
            &self.program_view,
            &atlas,
            &self.program_scene,
            resources,
            self.program_lighting,
        );

        // 比较实际链接代码；局部Guide/RG修改不能借用未变化材质叶子的身份复用旧管线。
        let code = [
            program_identity(
                programs
                    .artifacts()
                    .iter()
                    .chain(material_hits.iter().flat_map(|set| set.artifacts()))
                    .chain(primary_materials.iter().flat_map(|set| set.artifacts()))
                    .chain(primary.iter())
                    .chain(fused.iter())
                    .chain(fused_ser.iter())
                    .chain(realtime_graphs.iter())
                    .chain(reference_graphs.iter())
                    .chain(guide_hits.artifacts().iter().filter(|_| dispatch != 8)),
            ),
            program_identity(
                guide_hits
                    .artifacts()
                    .iter()
                    .chain(primary_guides.iter().flat_map(|set| set.artifacts()))
                    .chain(guided.iter())
                    .chain(guided_ser.iter()),
            ),
        ];
        let same_code = self.generation.as_ref().is_some_and(|previous| {
            hit == previous.hit && dispatch == previous.dispatch && code[0] == previous.code[0]
        });
        let same_guide_code = self.generation.as_ref().is_some_and(|previous| {
            hit == previous.hit && dispatch == previous.dispatch && code[1] == previous.code[1]
        });
        let pipelines_started = std::time::Instant::now();
        let (trace, post) = self.allocate_bindings(descriptors)?;
        let callables: Vec<_> = programs
            .artifacts()
            .iter()
            .map(|artifact| RayShader {
                spirv: artifact.bytes(),
                entry: c"shade_opaque",
            })
            .collect();
        let primary_callables: Vec<_> = primary_materials
            .iter()
            .flat_map(|set| set.artifacts())
            .map(|a| RayShader {
                spirv: a.bytes(),
                entry: c"shade_primary",
            })
            .collect();
        let guide_callables: Vec<_> = primary_guides
            .iter()
            .flat_map(|set| set.artifacts())
            .map(|a| RayShader {
                spirv: a.bytes(),
                entry: c"guide_primary",
            })
            .collect();
        let primary_shaders: Vec<_> = primary
            .iter()
            .zip([c"primary_random", c"primary_jittered"])
            .map(|(a, entry)| RayShader {
                spirv: a.bytes(),
                entry,
            })
            .collect();
        let configured = |reordered, reference| {
            let mut result = self::programs(&callables, reordered, hit);
            if dispatch != 0 {
                result.prefix = None;
                let code = if reference {
                    &reference_graphs[usize::from(reordered)]
                } else if reordered {
                    fused_ser.as_ref().expect("SER完整图")
                } else {
                    fused.as_ref().expect("完整图")
                };
                result.transport = RayShader {
                    spirv: code.bytes(),
                    entry: if reference {
                        c"render_diagnostic"
                    } else if dispatch == 8 {
                        c"render_offline"
                    } else {
                        c"render_fused"
                    },
                };
                result.material_hits = &hit_shaders;
                if dispatch >= 2 {
                    result.callables = &[];
                }
            }
            if dispatch == 8 && !reference {
                let base = usize::from(reordered) * if cfg!(test) { 3 } else { 2 };
                result.realtime = Some(RealtimePrograms {
                    primary: &primary_shaders,
                    transport_pair: [
                        RayShader {
                            spirv: realtime_graphs[base].bytes(),
                            entry: c"render_rt2_lane0",
                        },
                        RayShader {
                            spirv: realtime_graphs[base + 1].bytes(),
                            entry: c"render_rt2_lane1",
                        },
                    ],
                    merged: cfg!(test).then(|| RayShader {
                        spirv: realtime_graphs[base + 2].bytes(),
                        entry: c"render_rt2_merged",
                    }),
                    callables: &primary_callables,
                });
            }
            result
        };
        let regular = if same_code {
            self.generation.as_ref().unwrap().regular.rebind(
                &mut self.context,
                trace,
                post,
                [1, 1],
            )?
        } else {
            Renderer::new(
                &mut self.context,
                configured(false, false),
                trace,
                post,
                [1, 1],
            )?
        };
        let reordered = if self.context.capabilities().invocation_reorder {
            Some(
                if same_code
                    && let Some(previous) =
                        self.generation.as_ref().and_then(|g| g.reordered.as_ref())
                {
                    previous.rebind(&mut self.context, trace, post, [1, 1])?
                } else {
                    Renderer::new(
                        &mut self.context,
                        configured(true, false),
                        trace,
                        post,
                        [1, 1],
                    )?
                },
            )
        } else {
            None
        };

        let reference_regular = if cfg!(test) && dispatch == 8 {
            Some(Renderer::new(
                &mut self.context,
                configured(false, true),
                trace,
                post,
                [1, 1],
            )?)
        } else {
            None
        };
        let reference_reordered =
            if cfg!(test) && dispatch == 8 && self.context.capabilities().invocation_reorder {
                Some(Renderer::new(
                    &mut self.context,
                    configured(true, true),
                    trace,
                    post,
                    [1, 1],
                )?)
            } else {
                None
            };

        let guide_trace = self::programs(
            &[],
            false,
            materials::HitFeatures {
                authored_normals: false,
                ..hit
            },
        );
        let guide_groups: Vec<_> = std::iter::once(guide_trace.closest_hit)
            .chain(guide_hits.artifacts().iter().map(|a| RayShader {
                spirv: a.bytes(),
                entry: c"guide_hit",
            }))
            .collect();
        let prepare_guide = |context: &mut Context,
                             code: Option<&shader_build::Artifact>,
                             old: Option<RayTracingPipeline>| {
            code.map(|code| {
                if same_guide_code && let Some(old) = old {
                    old.rebind(context, trace)
                } else {
                    RayTracingPipeline::with_hit_groups(
                        context,
                        trace,
                        RayShader {
                            spirv: code.bytes(),
                            entry: c"guide_main",
                        },
                        guide_trace.miss,
                        &guide_groups,
                        guide_trace.any_hit,
                        &guide_callables,
                    )
                }
            })
            .transpose()
        };
        let guide_regular = prepare_guide(
            &mut self.context,
            guided.as_ref(),
            self.generation.as_ref().and_then(|g| g.guide_regular),
        )?;
        let guide_reordered = prepare_guide(
            &mut self.context,
            guided_ser.as_ref(),
            self.generation.as_ref().and_then(|g| g.guide_reordered),
        )?;

        eprintln!(
            "[prime_pt] shader_prepare complete tasks={} cache_hits={} compiled={} workers={} shader_ms={} pipeline_ms={} total_ms={} reuse_pt={} reuse_guides={} batch_main_bytes={} retained_main_bytes={}",
            compile_statistics.tasks,
            compile_statistics.cache_hits,
            compile_statistics.tasks - compile_statistics.cache_hits,
            compile_statistics.workers,
            compile_statistics.elapsed.as_millis(),
            pipelines_started.elapsed().as_millis(),
            prepare_started.elapsed().as_millis(),
            same_code,
            same_guide_code,
            batch_main_bytes,
            retained_main_bytes
        );
        Ok((
            resources,
            MaterialGeneration {
                dispatch,
                programs,

                hit,
                code,
                regular,
                reordered,
                reference_regular,
                reference_reordered,
                guide_regular,
                guide_reordered,
            },
        ))
    }
}
