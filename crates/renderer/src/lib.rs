//! 原生Vulkan渲染所有者；宿主导入、已完成命令和资源发布有独立边界。
//!
//! 实时PT分为首视线、确定性delta导引、随机光传输与后处理四段；离线由宿主固定输入后独立累积。
//! 整区无需覆盖判断时使用opaque几何；混合region的主射线通过any-hit判断覆盖，
//! 阴影RayQuery使用同一接受规则。当前路径不创建OMM。
//! 完整Hit Group路径在材质命中组内着色和查询阴影，交接为88B路径数值；
//! 要求设备启用EXT invocation reorder，SER执行仍可关闭；callable兼容路径的限制见[Gpu]。
//! RR导引与照明独立，反射和折射运动使用静态场景代理；适用范围见[Gpu]。
//! 材质世代预备全部程序和SBT；模式或尺寸变化只分配工作集、重绑descriptor。
//! 静态累积与RR分别显式失效，实际存储替换单独记录；随机身份、累积计数和jitter独立。
//! 默认最多12次表面反弹，末顶点保留发光和NEE；原生诊断关闭RR和跨帧RGB累积。
//! 每批随机样本基数只在完整记录经宿主编码、提交并完成后推进；失败和取消不接受新身份。

mod gpu;
mod reconstruction;
pub mod streamline;
pub use gpu::capture;
pub use gpu::{Error, GlobalReset, Gpu, Options, ResourceUpload, SceneUpdateTiming};
use prime_scene as scene;
