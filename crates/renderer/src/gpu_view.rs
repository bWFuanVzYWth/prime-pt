//! 尺寸相关工作集；完整分配成功后才替换当前目标。

use super::*;

impl RrImages {
    pub(super) fn new(
        context: &mut Context,
        extent: [u32; 2],
        output: [u32; 2],
        mode: Denoiser,
    ) -> Result<Self, Error> {
        let formats = [
            ImageFormat::Rgba16Float,
            ImageFormat::R32Float,
            ImageFormat::Rg32Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::R32Float,
            ImageFormat::Rg16Float,
        ];
        let images: [Image; 9] = formats
            .into_iter()
            .enumerate()
            .map(|(index, format)| {
                // 输入像素位移在半精度的 2048 像素以上已经无法逐像素表示。
                let format = match (mode, index) {
                    (Denoiser::Dlss, 8) => ImageFormat::Rg32Float,
                    (Denoiser::Nrd, 0 | 6) => ImageFormat::Rgba32Float,
                    _ => format,
                };
                let size = match (mode, index) {
                    (Denoiser::Dlss, 6) => output,
                    (Denoiser::Dlss, _) | (Denoiser::Nrd, 0 | 1 | 3..=8) => extent,
                    _ => [1, 1],
                };
                Image::new(context, [size[0], size[1], 1], format)
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .ok()
            .unwrap();
        let mask_extent = if mode == Denoiser::Dlss {
            extent
        } else {
            [1, 1]
        };
        // 同一光追管线含两种降噪器的入口。即使某帧不访问，storage 视图格式也必须匹配 SPIR-V。
        let nrd_motion = if mode == Denoiser::Nrd {
            [images[6], images[0]]
        } else {
            [Image::new(context, [1, 1, 1], ImageFormat::Rgba32Float)?; 2]
        };
        let (nrd_reflected_normal, rr_specular_motion) = if mode == Denoiser::Dlss {
            (
                Image::new(context, [1, 1, 1], ImageFormat::Rg16Float)?,
                images[8],
            )
        } else {
            (images[8], images[2])
        };
        Ok(Self {
            images,
            nrd_motion,
            nrd_reflected_normal,
            rr_specular_motion,
            disocclusion: Image::new(
                context,
                [mask_extent[0], mask_extent[1], 1],
                ImageFormat::R8Unorm,
            )?,
            previous_jitter: [0.; 2],
        })
    }

    pub(super) fn resources(&self, context: &Context) -> Result<[streamline::Resource; 9], Error> {
        let mut resources = [streamline::Resource::default(); 9];
        for (description, image) in resources.iter_mut().zip(self.images) {
            *description = Self::describe(context, image)?;
        }
        Ok(resources)
    }
    fn describe(context: &Context, image: Image) -> Result<streamline::Resource, Error> {
        let raw = image.raw(context)?;
        Ok(streamline::Resource {
            image: raw.image,
            view: raw.view,
            memory: raw.memory,
            width: raw.extent[0],
            height: raw.extent[1],
            format: raw.format,
            layout: raw.layout,
            usage: raw.usage,
            reserved: 0,
        })
    }
    pub(super) fn mask(&self, context: &Context) -> Result<streamline::Resource, Error> {
        Self::describe(context, self.disocclusion)
    }
}

impl View {
    pub(super) fn roots(&self) -> Vec<vulkan::native::Resource> {
        let mut roots = vec![
            self.params.into(),
            self.output.into(),
            self.accumulation.into(),
            self.counts.into(),
            self.prefix.into(),
            self.signal_lanes.into(),
            self.pack.into(),
        ];
        roots.extend(self.optical_history.map(vulkan::native::Resource::from));
        roots.extend(self.rr.nrd_motion.map(vulkan::native::Resource::from));
        roots.push(self.rr.nrd_reflected_normal.into());
        roots.push(self.rr.disocclusion.into());
        roots.extend(
            self.presentation
                .present
                .into_iter()
                .chain(self.presentation.resolve)
                .map(vulkan::native::Resource::from),
        );
        roots.extend(
            self.landing
                .iter()
                .copied()
                .map(vulkan::native::Resource::from),
        );
        roots.extend(
            self.rr
                .images
                .into_iter()
                .map(vulkan::native::Resource::from),
        );
        if let Some(bloom) = &self.presentation.bloom {
            roots.extend(bloom.roots());
        }
        roots.extend(self.presentation.display.roots());
        if let Some(nrd) = &self.nrd {
            nrd.roots(&mut roots);
        }
        roots
    }

    #[cfg(test)]
    pub(super) fn new(
        context: &mut Context,
        extent: [u32; 2],
        rr_enabled: bool,
        filter: &mut BloomFilters,
    ) -> Result<Self, Error> {
        Self::new_scaled(
            context,
            Some(filter),
            [extent; 2],
            if rr_enabled {
                Denoiser::Dlss
            } else {
                Denoiser::Off
            },
            streamline::Quality::Dlaa,
            None,
            None,
        )
    }

    pub(super) fn new_scaled(
        context: &mut Context,
        filter: Option<&mut BloomFilters>,
        [extent, output_extent]: [[u32; 2]; 2],
        mode: Denoiser,
        quality: streamline::Quality,
        previous_display: Option<&exposure::Display>,
        templates: Option<&Self>,
    ) -> Result<Self, Error> {
        if extent
            .into_iter()
            .chain(output_extent)
            .any(|n| n == 0 || n > 65536)
        {
            return Err(Error::Invalid("视图尺寸须属于 1..=65536"));
        }
        let pixels = u64::from(extent[0]) * u64::from(extent[1]);
        // 五个命中plane另加16B identity，供导引和照明共用首视线。
        let landing = [16, 16, 4, 8, 4, 16]
            .map(|stride| Buffer::new(context, pixels * stride, false))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let signal_lanes = Buffer::new(
            context,
            if mode == Denoiser::Nrd {
                pixels * vulkan::nrd::SIGNAL_BYTES_PER_PIXEL
            } else {
                vulkan::nrd::SIGNAL_BYTES_PER_PIXEL
            },
            false,
        )?;
        let rr_enabled = mode == Denoiser::Dlss;
        let optical_bytes = if mode != Denoiser::Off {
            pixels * 16
        } else {
            16
        };
        let optical_history = [
            Buffer::new(context, optical_bytes, false)?,
            Buffer::new(context, optical_bytes, false)?,
        ];
        let params = Buffer::new(context, 784, true)?;
        let output = Buffer::new(context, pixels * 16, false)?;
        let rr = RrImages::new(context, extent, output_extent, mode)?;
        let display = exposure::Display::new(
            context,
            output,
            extent,
            Some((
                rr.images[6],
                if rr_enabled { output_extent } else { [1, 1] },
            )),
            previous_display,
        )?;
        // 缓存 program 只引用自有占位资源；宿主 view 每帧通过 rebind 借出。
        let placeholder = Image::new(context, [1, 1, 1], ImageFormat::Rgba8Unorm)?;
        let linear_hdr = Image::new(
            context,
            [output_extent[0], output_extent[1], 1],
            ImageFormat::Rgba32Float,
        )?;
        let bloom = match filter {
            Some(filters) => filters.bind(context, linear_hdr)?,
            None => None,
        };
        let present_bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::SampledImage(linear_hdr),
                Binding::StorageImage(placeholder),
                Binding::StorageBuffer(display.state()),
                Binding::StorageBuffer(display.parameters()),
            ],
        )?;
        let present = display_variants(
            context,
            present_bindings,
            templates.map(|t| t.presentation.present),
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/present_present_manual.spv")),
                    c"present_manual",
                ),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/present_present_automatic.spv")),
                    c"present_automatic",
                ),
            ],
        )?;
        let bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::StorageBuffer(output),
                Binding::StorageImage(rr.images[0]),
                Binding::SampledImage(rr.images[6]),
                Binding::StorageImage(linear_hdr),
                Binding::SampledImage(rr.images[1]),
                Binding::SampledImage(rr.images[3]),
                Binding::SampledImage(rr.images[4]),
                Binding::SampledImage(rr.images[5]),
            ],
        )?;
        let pack_bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::StorageBuffer(output),
                Binding::StorageImage(rr.images[0]),
            ],
        )?;
        let pack = if let Some(templates) = templates {
            templates.pack.rebind(context, pack_bindings)?
        } else {
            ComputePipeline::new(
                context,
                pack_bindings,
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_pack.spv")),
                    c"pack_main",
                ),
            )?
        };
        let resolve = if let Some(templates) = templates {
            let [raw, rr, scaled] = templates.presentation.resolve;
            [
                raw.rebind(context, bindings)?,
                rr.rebind(context, bindings)?,
                scaled.rebind(context, bindings)?,
            ]
        } else {
            [
                ComputePipeline::new(
                    context,
                    bindings,
                    shader(
                        include_bytes!(concat!(env!("OUT_DIR"), "/resolve_raw.spv")),
                        c"resolve_raw",
                    ),
                )?,
                ComputePipeline::new(
                    context,
                    bindings,
                    shader(
                        include_bytes!(concat!(env!("OUT_DIR"), "/resolve_rr.spv")),
                        c"resolve_rr",
                    ),
                )?,
                ComputePipeline::new(
                    context,
                    bindings,
                    shader(
                        include_bytes!(concat!(env!("OUT_DIR"), "/resolve_scaled.spv")),
                        c"resolve_scaled",
                    ),
                )?,
            ]
        };
        Ok(Self {
            nrd: None,
            extent,
            output_extent,
            params,
            output,
            accumulation: Buffer::new(context, pixels * 16, false)?,
            counts: Buffer::new(context, pixels * 4, false)?,
            landing,
            signal_lanes,
            optical_history,
            prefix: Buffer::new(context, pixels * 16, false)?,
            progress: history::Accumulation::new(),
            jitter: [0.; 2],
            presentation: Presentation {
                params,
                present,
                resolve,
                linear_hdr,
                bloom,
                display,
            },
            rr_enabled,
            quality,
            rr,
            pack,
        })
    }
}

impl Presentation {
    pub(super) fn hdr(&self) -> Image {
        self.bloom
            .as_ref()
            .map_or(self.linear_hdr, vulkan::native::ImagePasses::output)
    }

    pub(super) fn record(
        &self,
        context: &mut Context,
        target: Binding,
        source: HdrSource,
        automatic: bool,
    ) -> Result<Recorded, Error> {
        let bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(self.params),
                Binding::SampledImage(self.hdr()),
                target,
                Binding::StorageBuffer(self.display.state()),
                Binding::StorageBuffer(self.display.parameters()),
            ],
        )?;
        let present = self.present[usize::from(automatic)].rebind(context, bindings)?;
        let [x, y, _] = self.linear_hdr.extent().map(|n| n.div_ceil(8));
        let mut commands = Commands::new(context)?;
        commands.barrier();
        commands.dispatch(self.resolve[source as usize], [x, y, 1])?;
        if let Some(bloom) = &self.bloom {
            bloom.record_into(&mut commands)?;
        } else {
            commands.barrier();
        }
        commands.dispatch(present, [x, y, 1])?;
        commands.barrier();
        Ok(commands.finish(context)?)
    }
}
