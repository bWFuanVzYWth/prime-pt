//! NRD 视图的输入、输出和私有历史共用同一资源世代。
use super::*;
use vulkan::nrd;

pub(super) struct NrdView {
    runtime: nrd::Runtime,
    images: [Image; 18],
    constants: Buffer,
    sun_radius_tan: f32,
    pack: ComputePipeline,
    resolve: ComputePipeline,
    post: ComputePipeline,
    pub(super) previous_jitter: [f32; 2],
    pub(super) light: Option<([f32; 3], bool, [f32; 2])>,
    pub(super) last_frame: std::time::Instant,
}
impl NrdView {
    #[cfg(test)]
    pub(super) fn diffuse_pair(&self) -> [Image; 2] {
        [self.images[3], self.images[9]]
    }
    #[cfg(test)]
    pub(super) fn specular_signals(&self) -> [Image; 3] {
        [self.images[5], self.images[6], self.images[11]]
    }
    #[cfg(test)]
    pub(super) fn shadow_pair(&self) -> [Image; 2] {
        [self.images[7], self.images[13]]
    }
    pub(super) fn new(
        context: &mut Context,
        view: &View,
        device: &streamline::Init,
        post: Bindings,
        radius: f32,
    ) -> Result<Self, Error> {
        let [w, h] = view.extent;
        let texture = |ctx: &mut Context, f| Image::new(ctx, [w, h, 1], f);
        let half = ImageFormat::Rgba16Float;
        let scalar = ImageFormat::R32Float;
        let rr = &view.rr.images;
        let images = [
            rr[6],
            texture(context, ImageFormat::Rgb10a2Unorm)?,
            rr[1],
            texture(context, half)?,
            texture(context, half)?,
            texture(context, half)?,
            texture(context, half)?,
            texture(context, scalar)?,
            texture(context, ImageFormat::R8Unorm)?,
            texture(context, half)?,
            texture(context, half)?,
            texture(context, half)?,
            texture(context, half)?,
            texture(context, scalar)?,
            rr[0],
            texture(context, ImageFormat::Rgb10a2Unorm)?,
            rr[7],
            texture(context, ImageFormat::R8Unorm)?,
        ];
        let mut external = [nrd::Image::new(0, 0); 18];
        for (dest, image) in external.iter_mut().zip(images) {
            let raw = image.raw(context)?;
            *dest = nrd::Image::new(raw.image, raw.format);
        }
        // SAFETY: Gpu 在同一宿主队列完成前保留视图；替换前回收上一帧，close 先释放 SDK。
        let runtime = unsafe {
            nrd::Runtime::new(
                [device.instance, device.physical_device, device.device],
                device.queue_family,
                device.queue_index,
                view.extent,
                external,
            )?
        };
        let constants = Buffer::new(context, 5 * 16, true)?;
        let sampled = Binding::SampledImage;
        let storage = Binding::StorageImage;
        let buffer = Binding::StorageBuffer;
        let make = |context: &mut Context,
                    indices: [usize; 4],
                    code,
                    entry|
         -> Result<ComputePipeline, Error> {
            let bindings = Bindings::new(
                context,
                &[
                    buffer(constants),
                    buffer(view.signal_lanes),
                    sampled(rr[1]),
                    sampled(rr[3]),
                    sampled(rr[4]),
                    sampled(rr[5]),
                    storage(images[1]),
                    storage(images[indices[0]]),
                    storage(images[indices[1]]),
                    storage(images[indices[2]]),
                    storage(images[indices[3]]),
                    storage(images[7]),
                    storage(images[8]),
                    sampled(images[13]),
                    buffer(view.output),
                    sampled(rr[7]),
                    sampled(rr[8]),
                    sampled(rr[0]),
                    storage(images[15]),
                    storage(images[17]),
                    sampled(rr[6]),
                ],
            )?;
            Ok(ComputePipeline::new(
                context,
                bindings,
                shader(code, entry),
            )?)
        };
        let pack = make(context, [3, 4, 5, 6], nrd::ADAPTERS[0], c"pack")?;
        let resolve = make(context, [9, 10, 11, 12], nrd::ADAPTERS[1], c"resolve")?;
        let post = ComputePipeline::new(
            context,
            post,
            shader(
                include_bytes!(concat!(env!("OUT_DIR"), "/post_nrd.spv")),
                c"post_nrd",
            ),
        )?;
        Ok(Self {
            runtime,
            images,
            constants,
            sun_radius_tan: radius.tan(),
            pack,
            resolve,
            post,
            previous_jitter: [0.; 2],
            light: None,
            last_frame: std::time::Instant::now(),
        })
    }
    pub(super) fn roots(&self, roots: &mut Vec<vulkan::native::Resource>) {
        roots.extend(self.images.map(vulkan::native::Resource::from));
        roots.extend([self.pack, self.resolve, self.post].map(vulkan::native::Resource::from));
        roots.push(self.constants.into());
    }
    pub(super) fn prepare(
        &self,
        context: &mut Context,
        extent: [u32; 2],
        camera: [f32; 16],
        jitter: [f32; 2],
    ) -> Result<Recorded, Error> {
        let mut words = [0; 20];
        words[..4].copy_from_slice(&[extent[0], extent[1], self.sun_radius_tan.to_bits(), 0]);
        words[4..].copy_from_slice(&camera.map(f32::to_bits));
        // 相机 right/up 的保留分量传递实际 film jitter。
        words[15] = jitter[0].to_bits();
        words[19] = jitter[1].to_bits();
        self.constants
            .write(context, 0, bytemuck::cast_slice(&words))?;
        let mut c = Commands::new(context)?;
        c.barrier();
        c.dispatch(self.pack, [extent[0].div_ceil(8), extent[1].div_ceil(8), 1])?;
        c.barrier();
        Ok(c.finish(context)?)
    }
    pub(super) unsafe fn evaluate(
        &mut self,
        command: u64,
        frame: &nrd::Frame,
    ) -> Result<(), Error> {
        // SAFETY: draw 独占正在录制的宿主命令并持有所有外部纹理。
        unsafe { self.runtime.record(command, frame)? };
        Ok(())
    }
    pub(super) fn compose(
        &self,
        context: &mut Context,
        extent: [u32; 2],
    ) -> Result<Recorded, Error> {
        let groups = [extent[0].div_ceil(8), extent[1].div_ceil(8), 1];
        let mut c = Commands::new(context)?;
        c.barrier();
        c.dispatch(self.resolve, groups)?;
        c.barrier();
        c.dispatch(self.post, groups)?;
        c.barrier();
        Ok(c.finish(context)?)
    }
}
