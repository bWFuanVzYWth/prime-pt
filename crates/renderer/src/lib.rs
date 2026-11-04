//! 原生Vulkan渲染所有者；宿主导入、已完成命令和资源发布有独立边界。
//!
//! 方块场景运行普通路径追踪。整区都无需覆盖判断时使用 opaque 几何；混合 region 的主射线
//! 通过 any-hit 判断覆盖，阴影 RayQuery 候选使用同一接受规则。当前渲染路径不创建 OMM。
//! RR 的反射和折射运动使用静态场景的代理，适用范围随 [Gpu] 的视图与历史契约维护。
mod gpu;
mod reconstruction;
pub mod streamline;
pub use gpu::{Error, GlobalReset, Gpu, Options, ResourceUpload, SceneUpdateTiming};
use prime_scene as scene;
