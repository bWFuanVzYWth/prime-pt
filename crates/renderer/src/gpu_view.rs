//! 尺寸相关工作集；完整分配成功后才替换当前目标。

use super::*;

impl RrImages {
    pub(super) fn new(
        context: &mut Context,
        extent: [u32; 2],
        output: [u32; 2],
    ) -> Result<Self, Error> {
        let formats = [
            ImageFormat::Rgba16Float,
            ImageFormat::R32Float,
            ImageFormat::Rg16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::Rgba16Float,
            ImageFormat::R32Float,
            ImageFormat::Rg16Float,
        ];
        let images = formats
            .into_iter()
            .enumerate()
            .map(|(index, format)| {
                let size = if index == 6 { output } else { extent };
                Image::new(context, [size[0], size[1], 1], format)
            })
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .ok()
            .unwrap();
        Ok(Self { images })
    }

    pub(super) fn resources(&self, context: &Context) -> Result<[streamline::Resource; 9], Error> {
        let raw = self
            .images
            .each_ref()
            .map(|image| image.raw(context))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(raw
            .into_iter()
            .map(|raw| streamline::Resource {
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
            .collect::<Vec<_>>()
            .try_into()
            .ok()
            .expect("固定九个图像"))
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
            self.pack.into(),
        ];
        roots.extend(
            self.present
                .into_iter()
                .chain(self.rr_present)
                .chain(self.rr_fallback)
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
        roots.extend(self.display.roots());
        roots
    }

    #[cfg(test)]
    pub(super) fn new(
        context: &mut Context,
        extent: [u32; 2],
        rr_enabled: bool,
    ) -> Result<Self, Error> {
        Self::new_scaled(
            context,
            extent,
            extent,
            rr_enabled,
            streamline::Quality::Dlaa,
            None,
            None,
        )
    }

    pub(super) fn new_scaled(
        context: &mut Context,
        extent: [u32; 2],
        output_extent: [u32; 2],
        rr_enabled: bool,
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
        let params = Buffer::new(context, 784, true)?;
        let output = Buffer::new(context, pixels * 16, false)?;
        let rr = RrImages::new(
            context,
            if rr_enabled { extent } else { [1, 1] },
            if rr_enabled { output_extent } else { [1, 1] },
        )?;
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
        let present_bindings = Bindings::new(
            context,
            &[
                Binding::StorageBuffer(params),
                Binding::StorageBuffer(output),
                Binding::StorageImage(placeholder),
                Binding::StorageBuffer(display.state()),
                Binding::StorageBuffer(display.parameters()),
            ],
        )?;
        let present = display_variants(
            context,
            present_bindings,
            templates.map(|t| t.present),
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
                Binding::StorageImage(placeholder),
                Binding::SampledImage(rr.images[1]),
                Binding::SampledImage(rr.images[3]),
                Binding::SampledImage(rr.images[4]),
                Binding::SampledImage(rr.images[5]),
                Binding::StorageBuffer(display.state()),
                Binding::StorageBuffer(display.parameters()),
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
        let rr_present = display_variants(
            context,
            bindings,
            templates.map(|t| t.rr_present),
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_present_manual.spv")),
                    c"present_manual",
                ),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_present_automatic.spv")),
                    c"present_automatic",
                ),
            ],
        )?;
        let rr_fallback = display_variants(
            context,
            bindings,
            templates.map(|t| t.rr_fallback),
            [
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_fallback_manual.spv")),
                    c"fallback_manual",
                ),
                shader(
                    include_bytes!(concat!(env!("OUT_DIR"), "/rr_fallback_automatic.spv")),
                    c"fallback_automatic",
                ),
            ],
        )?;
        Ok(Self {
            extent,
            output_extent,
            params,
            output,
            accumulation: Buffer::new(context, pixels * 16, false)?,
            counts: Buffer::new(context, pixels * 4, false)?,
            landing,
            prefix: Buffer::new(context, pixels * 16, false)?,
            progress: history::Accumulation::new(),
            jitter: [0.; 2],
            present,
            rr_enabled,
            quality,
            rr,
            pack,
            rr_present,
            rr_fallback,
            display,
        })
    }
}
