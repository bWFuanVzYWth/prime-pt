//! Prime PT 设置选择的两个引擎滤波器，选择只影响下一次视图绑定。
use simple_engine::{
    circular_bloom, fft_bloom,
    vulkan::native::{Context, Image, ImagePasses, Resource},
};

/// Prime PT 在显示变换前使用的镜头滤波器。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensMode {
    /// 使用完整工作台衍射核的 FFT 卷积。
    Diffraction(fft_bloom::Aperture),
    /// 以圆孔衍射为参考的多尺度空间泛光。
    FastCircular,
}

impl From<fft_bloom::Aperture> for LensMode {
    fn from(shape: fft_bloom::Aperture) -> Self {
        Self::Diffraction(shape)
    }
}

pub(crate) struct BloomFilters {
    fft: fft_bloom::gpu::Lens,
    circular: circular_bloom::Filter,
    mode: Option<LensMode>,
}

impl BloomFilters {
    pub(crate) fn new(ctx: &mut Context) -> Result<Self, fft_bloom::Error> {
        Ok(Self {
            fft: fft_bloom::gpu::Lens::new(ctx, fft_bloom::gpu::PROGRAMS)?,
            circular: circular_bloom::Filter::new(ctx)?,
            mode: Some(LensMode::FastCircular),
        })
    }
    pub(crate) const fn enabled(&self) -> bool {
        self.mode.is_some()
    }

    pub(crate) fn select(&mut self, mode: Option<LensMode>) -> bool {
        if self.mode == mode {
            return false;
        }
        match mode {
            Some(LensMode::Diffraction(shape)) => {
                self.fft.select(shape);
            }
            Some(LensMode::FastCircular) | None => self.fft.release_rectangle(),
        }
        self.mode = mode;
        true
    }

    pub(crate) fn bind(
        &mut self,
        ctx: &mut Context,
        input: Image,
    ) -> Result<Option<ImagePasses>, fft_bloom::Error> {
        match self.mode {
            Some(LensMode::Diffraction(_)) => self.fft.bind(ctx, input).map(|v| Some(v.into())),
            Some(LensMode::FastCircular) => Ok(Some(self.circular.bind(ctx, input)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn roots(&self) -> impl Iterator<Item = Resource> + '_ {
        self.fft.roots().chain(self.circular.roots())
    }
}
