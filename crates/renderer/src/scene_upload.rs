//! 属性补丁与灯目录共用一个可分帧的 PCIe 源，目标写入在整个候选就绪后一起排队。
use super::{Buffer, Error};
use simple_engine::scene_cache;

#[derive(Default)]
pub(super) struct Columns {
    pub bytes: Vec<u8>,
    pub copies: Vec<(Buffer, u64, u64, u64)>,
}
impl Columns {
    pub fn append(&mut self, buffer: Buffer, to: usize, bytes: &[u8]) -> Result<(), Error> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(scene_cache::Error::from)?;
        let from = self.bytes.len() as u64;
        self.copies
            .try_reserve(1)
            .map_err(scene_cache::Error::from)?;
        self.bytes.extend_from_slice(bytes);
        self.copies
            .push((buffer, from, to as u64, bytes.len() as u64));
        Ok(())
    }
    pub fn rows<T: bytemuck::Pod>(
        &mut self,
        buffer: Buffer,
        values: &[(usize, T)],
    ) -> Result<(), Error> {
        let mut sorted: Vec<_> = values.iter().collect();
        sorted.sort_unstable_by_key(|(i, _)| *i);
        for &(i, ref v) in sorted {
            self.append(buffer, i * size_of::<T>(), bytemuck::bytes_of(v))?;
        }
        Ok(())
    }
}
