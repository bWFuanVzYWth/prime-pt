//! Prime PT 的 Streamline 应用身份；通用接入由引擎 Vulkan crate 拥有。

pub(crate) use vulkan::streamline::{
    Error, EvaluationFailure, Frame, Init, Quality, Resource, shutdown,
};

const APPLICATION: vulkan::streamline::Application<'static> = vulkan::streamline::Application {
    engine_version: c"prime_pt",
    project_id: c"29d665c9-1f37-4e82-bd7b-5a17aac30bcd",
};

pub(crate) fn bootstrap() -> Result<(), Error> {
    vulkan::streamline::bootstrap(APPLICATION)
}

pub(crate) struct Runtime(vulkan::streamline::Runtime);

impl Runtime {
    /// # Safety
    /// 遵守 vulkan::streamline::Runtime::new 的设备和完成契约。
    pub(crate) unsafe fn new(init: Init) -> Result<Self, Error> {
        Ok(Self(unsafe {
            vulkan::streamline::Runtime::new(init, APPLICATION)?
        }))
    }

    pub(crate) fn configure(
        &mut self,
        output: [u32; 2],
        quality: Quality,
    ) -> Result<[u32; 2], Error> {
        self.0.configure(output, quality)
    }

    /// # Safety
    /// 遵守 vulkan::streamline::Runtime::evaluate 的命令和图像契约。
    pub(crate) unsafe fn evaluate(&mut self, frame: &Frame) -> Result<(), EvaluationFailure> {
        unsafe { self.0.evaluate(frame) }
    }
}
