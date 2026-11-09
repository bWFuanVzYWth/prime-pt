# 用 Nsight 抓取 Prime PT

这份手册用于 **Windows 上的 Nsight Graphics 2026.1**。先抓 GPU 时间线，定位 PT（路径追踪）或 RR（DLSS 光线重建）的耗时；需要分析寄存器时，再开启 shader（GPU 程序）采样。下面以源码位于 `C:\repositories\simple_engine\app\prime_pt` 为例。

## 1. 准备一次，然后复制三个输入框

先完成[源码环境准备](build.md#准备源码环境)，安装 [Nsight Graphics](https://developer.nvidia.com/nsight-graphics)，关闭已有的开发客户端。在 **PowerShell 7** 中复制执行：

```powershell
Set-Location 'C:\repositories\simple_engine\app\prime_pt'
.\scripts\prepare-nsight.ps1 -Offline
```

这个命令构建原生库并输出启动参数，**不会打开 Minecraft**。首次构建若提示缺少离线依赖，去掉 `-Offline` 重跑。源码在其他位置时，只改 `Set-Location` 那一行。

看到“已准备，尚未启动游戏”后，打开 Nsight Graphics，进入 **Start Activity / Connect**，选择 **GPU Trace Profiler**。按下面三个框填写；**每个代码块只复制内容，不加外层引号**。

**Application Executable**（启动程序）：

```text
C:\Windows\System32\cmd.exe
```

**Working Directory**（工作目录）：

```text
C:\repositories\simple_engine\app\prime_pt
```

**Command Line Arguments**（启动参数，整行复制，保留行内全部引号）：

```text
/d /s /c ""C:\repositories\simple_engine\app\prime_pt\gradlew.bat" --offline --no-daemon runClient "-PnativeProfile=release" "-PnativeLibrary=C:\repositories\simple_engine\app\prime_pt\target\release\prime_pt.dll""
```

如果源码位置、Windows 目录、构建档位或 `CARGO_TARGET_DIR` 与示例不同，**使用脚本输出的完整值**。脚本也将三个值分别写入：

| 文件，相对于 Prime PT 根目录 | 粘贴到 |
| --- | --- |
| `target/nsight-launch/application.txt` | Application Executable |
| `target/nsight-launch/directory.txt` | Working Directory |
| `target/nsight-launch/arguments.txt` | Command Line Arguments |

例如，要把实际启动参数放入剪贴板，在刚才的 PowerShell 窗口执行：

```powershell
Get-Content -Raw .\target\nsight-launch\arguments.txt | Set-Clipboard
```

不要把 `$primeRoot`、`$launchArgs` 等 PowerShell 变量名填进 GUI。`-PnativeLibrary` 已固定刚准备的 DLL，抓帧启动时不会再次构建 Rust。`--no-daemon` 避免使用未经过 Nsight 启动的旧 Gradle 守护进程。开发客户端会获得 Gradle 选定的 Slang 路径。

## 2. 采集选项照表填写

先用以下配置看耗时；需要看寄存器时只改最后一项。

| 界面选项 | 第一次看耗时 | 看寄存器和 shader 热点 |
| --- | --- | --- |
| Automatically Connect | **Yes** | 相同 |
| Start After | **Manual Trigger** | 相同 |
| Max Duration | **1000 ms** | 相同 |
| Limited To | **Max Frames**，数量 **3** | 相同 |
| GPU 时钟 | **Base** | 相同 |
| Multi-Pass Metrics | 关闭 | 关闭 |
| Time Every Action | 关闭 | 关闭 |
| Collect Shader Pipelines | 开启 | 开启 |
| Real-Time Shader Profiler | 关闭 | **开启** |

采集选项位于活动设置中，部分项目需要展开高级设置。第一列用于取得开销较小的时间基线；第二列用于定位 shader 内的热点。两种采集的开销不同，分别比较。[NVIDIA 采集选项说明](https://docs.nvidia.com/nsight-graphics/UserGuide/gpu-trace-overview.html#basic-workflow)

在 **Tools → Options** 修改捕获快捷键，例如设为 **Ctrl+Shift+F11**。默认 F11 会与 Minecraft 全屏切换冲突。然后回到启动页面，点击 **Launch**。

正常情况下，Nsight 会沿启动进程树连接实际绘图的游戏进程。若出现进程选择列表，选 **Minecraft 的 `java.exe`**，不要选命令行里只有 `GradleDaemon` 或 `GradleWrapperMain` 的进程。[NVIDIA 启动与连接说明](https://docs.nvidia.com/nsight-graphics/UserGuide/launch-application-overview.html#process-launch-and-connection-on-windows-targets)

## 3. 进入世界后再抓

进入要测量的世界，站在固定位置，等区块加载、资源准备及 Nsight 的后台编译结束。目标窗口中的采集状态应为 **Ready**，后台编译应为 **Inactive**。不要在标题页、暂停菜单或刚进入世界时抓稳态性能。

以 **3840×2160** 为基准：将游戏实际输出设为该尺寸，关闭垂直同步和帧率限制，固定视距、资源包和相机。RR 对照使用 **DLAA**，使输入尺寸也为 3840×2160；不要拿较低输入分辨率的 Quality/Performance 档直接比较 PT 耗时。截图或窗口名不是实际派发尺寸，应在报告中检查 `vkCmdTraceRaysKHR` 的 width/height。

在游戏聊天框中**逐条输入**以下命令，建立普通 PT、原生无累积的起点：

```text
/pt enabled on
/pt mode realtime
/pt bounces 12
/pt spp 1
/pt ser on
/pt diagnostic on
```

回到画面，等待稳定，然后按刚设好的捕获快捷键。保存为 `pt-native.ngfx-gputrace`。切换下一组后等待稳定，再抓一份：

| 要比较的组 | 在聊天框执行 | 建议文件名 |
| --- | --- | --- |
| 普通 PT + RR | 依次执行 `/pt diagnostic off`、`/pt quality dlaa`、`/pt rr on` | `pt-rr.ngfx-gputrace` |
| 同一组关闭 SER | `/pt ser off`；重新抓取后用 `/pt ser on` 恢复 | 在原文件名加 `-no-ser` |

`diagnostic on` 暂时关闭 RR 和时间累积，便于比较算法。做完原生对照后用 `diagnostic off` 恢复偏好。设置生效情况以游戏的 Prime PT 设置页和日志为准，详见[视频设置](settings.md)。离线模式持续累积普通 PT，不能替代这里的实时测量。

成功的报告应有实际世界画面、Vulkan 队列以及帧内光追或计算工作。只抓到菜单或空队列时，重新回到世界抓取。

## 4. 先看哪几个阶段

在时间线上选中一个完整帧，先看 **GPU 执行区间**，不要把 CPU API 调用时长当成 GPU 耗时。各队列有重叠时，不直接相加。

| 入口或工作 | 用途 |
| --- | --- |
| `guide_main` | RR 导引，包括法线、粗糙度、反照率和运动数据；与照明独立 |
| `render_fused` / `shade_hit` | 普通 PT 调度及材质照明 |
| RR / Streamline / DLSS 工作 | SDK 重建；与 Guide 和照明分别统计 |

分析 RR 对 PT 的影响时，将 **Guide、照明、后处理、SDK** 分别记录，另记整帧跨度。分析寄存器时选择实际活跃的 shader，区分“分配的寄存器数”和“活跃寄存器峰值”；寄存器多不等于已经证明发生 spill。找不到入口名时按实际管线和派发参数定位，不按调用序号猜测。

## 5. 日常迭代与保存证据

准备脚本默认使用 `release`，保留 Slang 调试信息，适合查看源代码和寄存器。要测发行构建，在 Prime PT 根目录的 PowerShell 执行：

```powershell
.\scripts\prepare-nsight.ps1 -Offline -Profile release-lto
```

随后重新复制 `arguments.txt`；旧参数仍指向旧档位。已有对应 DLL 时可以加 `-SkipBuild`，它只重新生成参数。修改 Rust 或 shader 后重新运行准备脚本，再由 Nsight 重启客户端；只重载资源不能更新编进 DLL 的固定入口。

开发编译日志中的 `shader_prepare complete` 分别报告 `shader_ms`（Slang 和缓存）、`pipeline_ms`（驱动管线准备）以及命中数量。无修改时应复用缓存；不要为了“确保更新”每次删除 `target/shaders` 或 `target/pipeline-cache`。[编译与缓存说明](build.md#运行开发客户端)

将 `.ngfx-gputrace`、`run/logs/latest.log`、`target/nsight-launch/launch.json` 和本次源码版本一起保存到 `temp/` 的新目录。另记实际输入/输出尺寸、GPU/驱动、世界位置、SER/RR 状态和采集选项；保留对应 DLL 与 shader 缓存。`target/` 仅存可再生产物，不能作为现场证据的唯一副本。已有文件的批量分析见引擎[离线读取手册](../../../manual/nsight.md)。

## 常见问题

| 现象 | 处理 |
| --- | --- |
| Launch 后立即退出 | 先看启动终端和 `run/logs/latest.log`；确认三个框使用脚本输出的完整值，参数整行且引号未丢失。 |
| 只有 Gradle，没有游戏 | 等待 Java 端准备完成；失败时先解决日志中的构建/依赖错误。不要反复 Attach 普通终端启动的旧客户端。 |
| 仍然加载旧 DLL | 重新运行准备脚本并复制 `arguments.txt`，核对 `launch.json` 中的路径与 SHA-256。 |
| 找不到 Slang | 确认固定的 Slang 发行包已安装，或在准备前设置 `SLANGC`；自定义环境也应在启动 Nsight 的环境中可见。 |
| F11 改变了全屏或分辨率 | 改捕获快捷键，恢复目标尺寸，回到世界预热后重新抓。 |
| 无权读取 GPU 计数器 | 按 NVIDIA 的[计数器权限说明](https://developer.nvidia.com/ERR_NVGPUCTRPERM)配置。缺失指标不能记为零。 |
| 开启 shader 采样后更慢 | 与未开启采样的基线分开比较；采集本身也有带宽和内存成本。 |
| 验证错误、注入或回放失败 | 保留完整错误和版本。GPU Trace 与 Graphics Capture 是不同活动；回放失败不能当作渲染验证通过。 |
