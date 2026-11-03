use crate::{Error, MAX_PACKET};

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_PACKET {
            return Err(Error::Input("Minecraft源包超过256MiB"));
        }
        Ok(Self { bytes, at: 0 })
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self
            .at
            .checked_add(length)
            .ok_or(Error::Input("源范围溢出"))?;
        let value = self
            .bytes
            .get(self.at..end)
            .ok_or(Error::Input("源包截断"))?;
        self.at = end;
        Ok(value)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("固定四字节"),
        ))
    }

    pub(crate) fn i32(&mut self) -> Result<i32, Error> {
        Ok(self.u32()? as i32)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("固定八字节"),
        ))
    }

    pub(crate) fn f32(&mut self) -> Result<f32, Error> {
        let value = f32::from_bits(self.u32()?);
        if !value.is_finite() {
            return Err(Error::Input("源浮点不是有限值"));
        }
        Ok(value)
    }

    pub(crate) fn f64(&mut self) -> Result<f64, Error> {
        let value = f64::from_bits(self.u64()?);
        if !value.is_finite() {
            return Err(Error::Input("源浮点不是有限值"));
        }
        Ok(value)
    }

    pub(crate) fn count(&mut self, stride: usize) -> Result<usize, Error> {
        let count = self.u32()? as usize;
        if count
            .checked_mul(stride)
            .is_none_or(|bytes| bytes > self.bytes.len() - self.at)
        {
            return Err(Error::Input("源数量超过剩余数据"));
        }
        Ok(count)
    }

    pub(crate) fn string(&mut self) -> Result<String, Error> {
        let count = self.count(1)?;
        Ok(String::from_utf8(self.take(count)?.to_vec())?)
    }

    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.at != self.bytes.len() {
            return Err(Error::Input("源包含未消费尾部"));
        }
        Ok(())
    }
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }
}
