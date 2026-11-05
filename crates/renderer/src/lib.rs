//! 原生Vulkan渲染所有者；宿主导入、已完成命令和资源发布有独立边界。
//!
//! 方块场景可运行普通路径追踪或 point ReSTIR PT；离线由宿主固定输入后独立累积普通 PT。
//! 整区都无需覆盖判断时使用 opaque 几何；混合 region 的主射线
//! 通过 any-hit 判断覆盖，阴影 RayQuery 候选使用同一接受规则。当前渲染路径不创建 OMM。
//! RR 的反射和折射运动使用静态场景的代理，适用范围随 [Gpu] 的视图与历史契约维护。
//! ReSTIR按方向并行处理双向时间shift与三组paired邻居，材质叶子按Flow/操作特化并预备SER；
//! 复用初采与pairwise MIS；场景、材质、光照及
//! 路径支持变化保守重置整个复用域。屏幕工作集按 16×16 tile 补齐，约 260B/像素，
//! 另有每几何槽 64B 的局部四角/region 键和 768KiB 邻居表；普通 PT 不保留恢复几何或全屏复用工作集。
//! 材质世代准备全部已知程序和 SBT；模式或尺寸变化只分配工作集、重绑 descriptor，不懒编译 shader。
//! 固定fresh概率只选择当前输出，不反馈路径历史；历史M上限只应用于temporal MIS。
//! 原生诊断保留所选积分器及内部复用，关闭RR、强制1 spp并跳过RGB累积读写。
mod gpu;
mod reconstruction;
pub mod streamline;
pub use gpu::{Error, GlobalReset, Gpu, Options, ResourceUpload, SceneUpdateTiming};
use prime_scene as scene;
