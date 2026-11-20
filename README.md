# Prime PT

Minecraft Java 版的路径追踪 mod，基于 Vulkan 硬件光追管线。

## 特点

- 写实风格，追求物理正确与色彩还原
- 日光穿过地形和镂空纹理时产生体积光，挖开遮挡后光照会随帧更新
- 部分支持 Lab PBR 标准的资源包
- 实时普通 PT，每像素固定两条路径。离线快照固定场景和相机后按指定批大小持续累积。两种模式均使用平方根俄罗斯轮盘赌

## 需求

| 项目 | 要求 |
| --- | --- |
| 系统和处理器 | Windows x64。当前源码的 Windows 发行构建默认要求支持 `x86-64-v3`，这组 CPU 指令包含 AVX2 等。 |
| 游戏与加载器 | Minecraft Java 版 **26.3**、Java **25** 和 Fabric Loader **0.19.5 或更新版本**。Fabric Loader 负责让游戏加载 Mod。 |
| 显卡与驱动 | 能运行 Vulkan **1.4**，并支持硬件光追管线、射线查询（RayQuery）、`VK_KHR_ray_tracing_position_fetch`、`VK_KHR_device_address_commands` 和可请求32线程组的 `subgroupSizeControl`。详细能力见[设备检查](manual/build.md#检查显卡与驱动)。 |
| 启动方式 | 把 JAR 放入该游戏实例的 `mods/`，并以 `--graphicsBackend VULKAN` 启动。具体步骤见[安装发行包](manual/build.md#安装发行包)。 |
| 可选的画面重建 | DLSS RR 需要支持该功能的 NVIDIA 显卡和驱动。设备缺少 RR 能力时可用原始路径追踪，基础 Vulkan 光追仍是必需条件。当前不提供 DLSS 帧生成。 |

发行包内含 Slang 编译器、原生运行库和相关资源。手上已有 JAR 时，按游戏安装步骤即可。若要从源码构建或修改 Mod，则需要按目录放置 simple_engine 源码，并准备 Java 25 JDK、仓库固定的 Rust nightly、PowerShell 7、MSVC x64 C++ 构建工具、Slang 和 Streamline SDK。步骤见[源码构建](manual/build.md#准备源码环境)。

## 文档

- [安装与运行](manual/build.md)
- [视频设置](manual/settings.md)
- [诊断手册](manual/diagnostics.md)
- [Nsight 抓帧手册](manual/nsight.md)
- [开发手册](manual/README.md)

## 协议

项目代码采用 [GPL-3.0-only](LICENSE)。素材和第三方组件的声明见 [NOTICE](NOTICE) 和 [原版 PBR 资产许可](crates/minecraft/assets/vanilla_pbr/SOURCE-LICENSE.md)。
