//! 原生Vulkan渲染所有者；宿主导入、已完成命令和资源发布有独立边界。
//!
//! 方块场景可运行普通路径追踪或 point ReSTIR PT；离线由宿主固定输入后独立累积普通 PT。
//! 整区都无需覆盖判断时使用 opaque 几何；混合 region 的主射线
//! 通过 any-hit 判断覆盖，阴影 RayQuery 候选使用同一接受规则。当前渲染路径不创建 OMM。
//! RR 的反射和折射运动使用静态场景的代理，适用范围随 [Gpu] 的视图与历史契约维护。
//! 普通PT采用材质Hit Group与raygen阴影RayQuery；100B交接只携带当前材质求值所需数据。
//! ReSTIR按方向并行处理双向时间shift与三组paired邻居；已知表面仍用Flow/操作特化callable，
//! 不统一不同来源的材质解码。初采对太阳和局部灯各采一次，继续使用pairwise MIS。
//! 时间状态按累积、RR、路径和ReSTCV颜色分别显式失效；相机、局部编辑、照明与路径预算变化
//! 不推导整个重投影域失效。实际存储替换单独记录，随机样本基数与累积计数和jitter独立。
//! 屏幕工作集按16×16 tile补齐；方向融合和CV的准确容量由
//! [`Layout::memory`](simple_engine::restir_pt::Layout::memory)计算，不含应用的几何目录与guide。
//! 应用另保存每像素12B的当帧CV反射率代理，避免把材质读取带入合并阶段。
//! 材质世代准备全部已知程序和SBT；模式或尺寸变化只分配工作集、重绑descriptor，不懒编译shader。
//! 固定fresh概率只选择当前输出，不反馈路径历史；历史M上限只应用于temporal MIS。
//! 两种积分器默认最多12次表面反弹，末顶点保留发光和NEE；到达上限后不再散射。
//! 原生诊断保留所选积分器、内部复用及每帧SPP，关闭RR与跨帧RGB累积。
//! 每批随机样本基数只在完整记录经宿主编码、提交并完成后推进；失败和取消不接受新身份。

mod gpu;
mod reconstruction;
pub mod streamline;
pub use gpu::{Error, GlobalReset, Gpu, Options, ResourceUpload, SceneUpdateTiming};
use prime_scene as scene;
