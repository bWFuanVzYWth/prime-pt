//! 原生 HDR 测光适配；算法、工作集和时序规则来自引擎 display::exposure。

use simple_engine::display::exposure::{
    Frame,
    gpu::{Exposure, Programs},
};
use simple_engine::vulkan::{
    self,
    interop::RayShader,
    native::{Binding, Buffer, Context, Image, Recorded, Resource},
};

pub(super) struct Display {
    parameters: Buffer,
    raw: Exposure,
    reconstructed: Option<Exposure>,
}

fn programs(texture: bool) -> Programs<'static> {
    Programs {
        histogram: RayShader {
            spirv: if texture {
                include_bytes!(concat!(env!("OUT_DIR"), "/exposure.texture.spv"))
            } else {
                include_bytes!(concat!(env!("OUT_DIR"), "/exposure.buffer.spv"))
            },
            entry: c"histogram_main",
        },
        update: RayShader {
            spirv: include_bytes!(concat!(env!("OUT_DIR"), "/exposure.update.spv")),
            entry: c"update_main",
        },
    }
}

impl Display {
    pub(super) fn new(
        context: &mut Context,
        input: Buffer,
        extent: [u32; 2],
        reconstruction: Option<(Image, [u32; 2])>,
        previous: Option<&Self>,
    ) -> Result<Self, vulkan::Error> {
        let parameters = match previous {
            Some(previous) => previous.parameters,
            None => Buffer::new(context, 32, true)?,
        };
        let raw = match previous {
            Some(previous) => previous.raw.rebind(
                context,
                Binding::StorageBuffer(input),
                Binding::StorageBuffer(parameters),
                extent,
            )?,
            None => Exposure::new(
                context,
                programs(false),
                Binding::StorageBuffer(input),
                Binding::StorageBuffer(parameters),
                extent,
            )?,
        };
        let reconstructed = reconstruction
            .map(|(image, extent)| {
                if let Some(previous) =
                    previous.and_then(|previous| previous.reconstructed.as_ref())
                {
                    previous.rebind(
                        context,
                        Binding::SampledImage(image),
                        Binding::StorageBuffer(parameters),
                        extent,
                    )
                } else {
                    raw.variant(
                        context,
                        programs(true),
                        Binding::SampledImage(image),
                        Binding::StorageBuffer(parameters),
                        extent,
                    )
                }
            })
            .transpose()?;
        Ok(Self {
            parameters,
            raw,
            reconstructed,
        })
    }

    pub(super) const fn state(&self) -> Buffer {
        self.raw.state()
    }

    pub(super) const fn parameters(&self) -> Buffer {
        self.parameters
    }

    pub(super) fn roots(&self) -> Vec<Resource> {
        let mut roots = self.raw.roots().to_vec();
        if let Some(reconstructed) = &self.reconstructed {
            roots.extend(reconstructed.roots());
        }
        roots
    }

    pub(super) fn record(
        &self,
        context: &mut Context,
        frame: Frame,
        reconstructed: bool,
        enabled: bool,
    ) -> Result<Option<Recorded>, vulkan::Error> {
        self.parameters
            .write(context, 0, bytemuck::cast_slice(&frame.to_words()))?;
        if !enabled {
            return Ok(None);
        }
        let exposure = if reconstructed {
            self.reconstructed
                .as_ref()
                .expect("RR视图已建立texture测光")
        } else {
            &self.raw
        };
        exposure.record(context).map(Some)
    }
}
