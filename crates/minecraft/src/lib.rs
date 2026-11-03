//! Minecraft 26.3压缩源协议及纯CPU编译；不持有Java指针、线程池或GPU资源。
//!
//! 资源字典与section源分别拥有明确世代。输入先校验再建立独占存储，编译只借用
//! 已接受字典与邻域；缺失资源或未知邻域不伪装为空。批量并行和缓存由world调度。
#![forbid(unsafe_code)]
#![feature(portable_simd)]

mod biome;
mod biome_source;
pub mod tint;
mod tint_cache;

mod compile;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
mod fluid;
mod materials;
mod palette;
mod resources;
#[cfg(test)]
mod resources_tests;
mod vanilla;
pub mod vanilla_frames;
mod wire;

pub use compile::{
    CompiledSection, Neighbor, Neighborhood, TintPatch, TintRequest, compile_section,
    compile_section_slab, compile_section_slab_limited,
};
pub use materials::MaterialResources;
pub use palette::{SectionSource, SectionStorage, decode_sections};
pub use resources::{Catalog, ImageSource, PreparedCatalog, ResourceChange, SpriteSource};

/// 原始压缩源与资源协议的最大单包字节数，不是总场景内存预算。
pub const MAX_PACKET: usize = 256 * 1024 * 1024;

/// 源协议、资源世代或编译失败；保留可恢复的底层错误来源。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 长度、索引、版本或语义不满足协议。
    #[error("{0}")]
    Input(&'static str),
    /// 源资源名称不是UTF-8。
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
    /// 显式源存储扩容失败。
    #[error(transparent)]
    Allocation(#[from] std::collections::TryReserveError),
    /// 必要的邻域源尚未可用；world保留旧覆盖并延后，不当作air。
    #[error("必要的section邻域尚未可用：{0:?}")]
    MissingNeighbor([i32; 3]),
    /// 自持输出超过调度层额度，required为下一项输出至少需要的quad数。
    #[error("编译输出超过额度，至少需要{required}个quad")]
    OutputLimit {
        /// 下一项输出所需的最小quad总数。
        required: usize,
    },
    /// 内嵌原版材质PNG无效。
    #[error(transparent)]
    Png(#[from] png::DecodingError),
    /// 通用静态发光面积或alias输入无效。
    #[error(transparent)]
    Light(#[from] light_sampling::Error),
}
