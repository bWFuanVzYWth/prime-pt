//! 几何编译留下的紧凑tint来源；纯颜色刷新借旧几何，不重新选择模型或复制Quad。
use crate::{Catalog, Error, TintRequest};

/// 同一slab中连续quad的真实tint来源；16B，不保存Java引用或重复绝对坐标。
/// source仅在同一Catalog世代解释；资源替换必须废弃所有旧绑定。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TintSpan {
    /// 本slab局部首quad索引。
    pub first: u32,
    /// 非零连续quad数，不能超出实际mesh。
    pub count: u32,
    /// 资源准备时发行的非零tint source ID。
    pub source: u32,
    /// section内block位置，位序x|z<<4|y<<8，仅低12bits有效。
    pub local: u16,
    /// 当前协议必须为零。
    pub reserved: u16,
}
const _: () = assert!(size_of::<TintSpan>() == 16);
impl TintSpan {
    /// 校验输入范围；绑定来源的世代与source身份仍由Catalog解释入口验证。
    pub fn validate(self, quad_count: usize) -> Result<(), Error> {
        if self.count == 0
            || self.source == 0
            || self.local >= 4096
            || self.reserved != 0
            || u64::from(self.first) + u64::from(self.count) > quad_count as u64
        {
            return Err(Error::Input("tint span来源或quad范围无效"));
        }
        Ok(())
    }
}
impl Catalog {
    /// 本资源表source是否为任意宿主回退；调用者须先验证World/Catalog同世代。
    pub fn tint_is_custom(&self, source: u32) -> Result<bool, Error> {
        self.tints()
            .ok_or(Error::Input("tint绑定缺少准备资源"))?
            .is_custom_source(source)
    }
    /// 在明确资源世代内恢复真实宿主/原版tint输入，不从UV或MaterialId猜来源。
    /// section为绝对16m坐标；溢出、旧世代、无效source/local/reserved均拒绝。
    pub fn tint_request(
        &self,
        generation: u64,
        section: [i32; 3],
        binding: TintSpan,
    ) -> Result<TintRequest, Error> {
        if generation == 0
            || generation != self.generation()
            || binding.local >= 4096
            || binding.reserved != 0
        {
            return Err(Error::Input("tint绑定资源世代或local位置无效"));
        }
        let local = [
            binding.local & 15,
            binding.local >> 8,
            (binding.local >> 4) & 15,
        ];
        let mut position = [0; 3];
        for axis in 0..3 {
            position[axis] = section[axis]
                .checked_mul(16)
                .and_then(|v| v.checked_add(i32::from(local[axis])))
                .ok_or(Error::Input("tint绑定绝对位置溢出"))?;
        }
        self.tints()
            .ok_or(Error::Input("tint绑定缺少准备资源"))?
            .source_request(binding.source, position)
    }
}
