# 用 Nsight 抓取 Prime PT 的 GPU 帧

排查普通 PT、ReSTIR 或 RR 的耗时时，先用 **NVIDIA Nsight Graphics 的 GPU Trace Profiler**。它把显卡执行的工作放在时间轴上，适合查看光追、计算、复制和等待分别占了多少时间。下文以 Windows、PowerShell 7 和 Nsight Graphics 2026.1 为例；命令参数已对照该版本的 `ngfx.exe --help-all`。实际游戏抓取由操作者执行。

## 最短路径：从 Nsight 启动开发客户端

先完成[源码环境准备](build.md#准备源码环境)，安装 [Nsight Graphics](https://developer.nvidia.com/nsight-graphics)。关闭正在运行的开发客户端，在含有 `gradlew.bat` 的 Prime PT 根目录打开 PowerShell。以下构建命令只准备原生库，不启动游戏。

```powershell
Set-Location 'D:/projects/simple_engine/app/prime_pt'
./gradlew.bat --offline buildNative
if ($LASTEXITCODE -ne 0) { throw '原生库构建失败，先检查终端输出' }

$primeRoot = (Get-Location).Path
$nativeDll = (Resolve-Path './target/release-lto/prime_pt.dll').Path
$gradleBat = (Resolve-Path './gradlew.bat').Path
$nsightHost = Join-Path $env:ProgramFiles 'NVIDIA Corporation/Nsight Graphics 2026.1.0/host/windows-desktop-nomad-x64'
$ngfx = Join-Path $nsightHost 'ngfx.exe'
& $ngfx --version

$captureDir = Join-Path $primeRoot "temp/nsight/$((Get-Date).ToString('yyyyMMdd-HHmmss-fff'))"
New-Item -ItemType Directory -Path $captureDir -ErrorAction Stop | Out-Null
$launchArgs = '/d /s /c ""{0}" --offline --no-daemon runClient "-PnativeLibrary={1}""' -f $gradleBat, $nativeDll
```

修改第一行的源码目录；Nsight 安装版本不同时修改 `$nsightHost`。`--offline` 要求依赖已缓存，首次准备可去掉。若使用自定义 `CARGO_TARGET_DIR`，将 `$nativeDll` 改为构建实际输出的 DLL。`-PnativeLibrary` 让本次运行使用刚确认的库，避免抓帧启动期间重新构建。

在**同一终端**执行下面的命令。此步会通过 Nsight 启动 Minecraft，终端等待手动触发采集。

```powershell
& $ngfx --activity 'GPU Trace Profiler' --platform Windows `
    --exe "$env:SystemRoot/System32/cmd.exe" --dir $primeRoot --args $launchArgs `
    --start-after-hotkey --limit-to-frames 3 --max-duration-ms 1000 `
    --architecture Ada --metric-set-id 1 --set-gpu-clocks base `
    --output-dir $captureDir --no-timeout
```

`Ada` 对应 RTX 40 系列；其他显卡先执行 `& $ngfx --help-all`，按实际架构选择指标集。这里采集最多3个呈现帧，最长1000毫秒，使用基础时钟；没有启用 Shader Profiler、多轮指标或逐调用强制计时。不要将此时的 FPS 与正常运行的 FPS 直接对比。[NVIDIA GPU Trace 说明](https://docs.nvidia.com/nsight-graphics/UserGuide/gpu-trace-overview.html)

进入同一个世界，等待区块加载和首次管线准备结束，站定观察固定画面。确认 Nsight 已连接并准备好后，使用其捕获快捷键。默认 **F11** 与 Minecraft 全屏键冲突，应先在 Nsight 的 **Tools → Options** 中改为不冲突的键，再启动这次采集。[捕获快捷键说明](https://docs.nvidia.com/nsight-graphics/UserGuide/after-launch.html#target-application-capture-hotkey)

成功后，输出目录应有非空的 `.ngfx-gputrace`。用 Nsight Graphics 打开它，确认报告包含游戏画面的截图、Vulkan 队列及帧内光追或计算工作。只有标题界面或没有游戏 GPU 工作的文件，不能用来判断世界渲染成本。

## 使用图形界面启动

也可以在 Nsight Graphics 的 **Start Activity / Connect** 界面选择 **GPU Trace Profiler**，填入与上面相同的配置：

| 项目 | 填写内容 |
| --- | --- |
| Application Executable | `$env:SystemRoot/System32/cmd.exe` 展开后的路径 |
| Working Directory | `$primeRoot` 的值，即 Prime PT 根目录 |
| Command Line Arguments | `$launchArgs` 的完整值，可在 PowerShell 输入 `$launchArgs` 查看 |
| Automatically Connect | Yes |
| Start After | Manual Trigger |
| 采集范围 | 最多3帧、最长1000毫秒 |
| GPU 时钟 | Base，所有对照保持一致 |
| Real-Time Shader Profiler / Multi-Pass Metrics / Time Every Action | 第一轮均关闭 |

从准备好 Slang、SDK 和 Java 的终端启动 `ngfx-ui.exe`，可让它继承该终端环境。点击 Launch 后再进入世界。Windows 的 Nsight 启动器会寻找进程树中实际执行图形工作的进程；本项目用 `--no-daemon` 创建本次启动独有的 Gradle 进程，避免复用未经过 Nsight 启动的旧守护进程。[NVIDIA 启动与连接说明](https://docs.nvidia.com/nsight-graphics/UserGuide/launch-application-overview.html)

连接列表里应选择实际游戏的 `java.exe`，其命令行包含 Fabric 客户端启动入口；不要选只有 `GradleDaemon` 或 `GradleWrapperMain` 的进程。可在另一个 PowerShell 窗口核对：

```powershell
Get-CimInstance Win32_Process -Filter "Name = 'java.exe'" |
    Select-Object ProcessId, ParentProcessId, CommandLine | Format-List
```

Attach 用于连接已经通过 Nsight 启动、注入的进程。普通终端直接运行的旧 `runClient` 不能当作这条流程的起点；退出后从 Nsight 重新启动。

## 先抓这几组对照

在游戏聊天框逐条输入。先固定窗口大小、视距、资源包、世界时间、相机位置和顶点预算，关闭垂直同步及帧率限制。保持世界在绘制，不要停在暂停菜单。设置说明见[视频设置](settings.md)。

```text
/pt enabled on
/pt mode realtime
/pt bounces 12
/pt spp 1
/pt ser on
/pt diagnostic on
/pt renderer path_trace
```

第一组是原生分辨率、每帧1个样本、关闭RR及跨帧图像累积的普通 PT；这是上面的 `/pt spp 1` 所设，不是诊断模式强制。改为 `/pt spp 4` 可对照固定4 SPP。站定预热后抓两次，保存为 `pt-raw-a1`、`pt-raw-a2`，确认同版本重复采集的差异，再切到 ReSTIR：

```text
/pt renderer restir_pt
/pt mcap 20
/pt fresh 40
```

等待复用历史稳定，抓两次 `restir-raw-b1`、`restir-raw-b2`。诊断模式仍保留 ReSTIR 的内部路径复用、曝光和色调映射，关闭的是跨帧的图像样本累积。若想查看完全初采的输出，将 `fresh` 设为100；这不会省略复用阶段，不能用来估计关闭复用后的成本。

| 要区分的问题 | 对照方法 |
| --- | --- |
| 积分器自身成本 | 上述普通 PT 与 ReSTIR，均开启诊断；记录实际 TraceRays/Dispatch 尺寸。 |
| SER 是否有收益 | 同一积分器分别 `/pt ser off`、`/pt ser on`；每次等切换完成再抓。 |
| RR 与输入分辨率的影响 | `/pt diagnostic off`，分别 `/pt rr off`、`/pt rr on`；RR开启时固定相同 `/pt quality` 档位。RR可能同时改变输入尺寸和增加重建工作，不能把总差值全部归为降噪成本。 |
| 画面抖动与历史复用 | 固定相机，比较诊断模式、RR关闭及RR开启；另存连续画面或录像。单帧时间报告不能证明时序画面稳定。 |
| 场景上传或加速结构更新 | 将站定帧与移动、编辑方块后的帧分别抓取；记录操作和时间。 |
| 原版世界是否恢复 | `/pt enabled off` 后单独抓取，确认有原版世界绘制。 |

做完后用 `/pt diagnostic off` 恢复原来的 RR、画质和 SPP 偏好。不要用离线快照替代这里的 ReSTIR 对照：离线模式固定场景并运行普通 PT，持续累积图像。

## 打开报告后看什么

先在时间轴选中一个完整的世界渲染帧，再查看该帧的 GPU 工作。**CPU API 调用持续时间、GPU 队列执行区间和整帧跨度分别读取**；不同队列的重叠区间不能直接相加。先找长工作和空闲间隙，再展开 shader。

| 报告中的工作或 shader 入口 | Prime PT 中的用途与检查方向 |
| --- | --- |
| `prefix_main`、`render_main` | 普通 PT 的前缀与路径采样；核对输入尺寸、每帧样本量、顶点预算及SER状态。 |
| `restir_initial` | ReSTIR 初始路径候选。 |
| `restir_temporal_direction`、`restir_temporal_combine` | 前帧路径重连、双方向计算及合并。 |
| `restir_spatial_work`、`restir_spatial_direction`、`restir_merge` | 邻域候选、方向计算、空间合并及输出。 |
| `shade_opaque`、`restir_initial_flow`、`restir_replay_flow`、`restir_evaluate_flow` | 光追调用的材质程序；ReSTIR额外重放、求值的成本也要计入。 |
| `post_main` 及显示、曝光、天空相关计算 | 图像解析和显示处理；诊断模式仍有这些工作。 |
| RR / NGX 相关工作 | 关闭诊断、实际启用RR后才应出现；按实际报告定位，不假定一定显示SDK名称。 |
| Buffer/Image复制、BLAS/TLAS构建 | 场景上传、几何与加速结构更新；检查是否出现在本应稳定的帧。 |

入口名称与可见的 pipeline 名称受调试信息和工具版本影响。找不到名称时先按实际管线、shader及派发参数定位，不按序号猜测。SM利用率、显存流量、寄存器数和warp等待原因可以帮助缩小范围；单个高百分比不足以证明瓶颈，也不能把寄存器多直接写成发生了spill（寄存器溢出到内存）。

GPU 持续繁忙且某阶段很长，优先查对应 shader 和读写量；GPU 存在长空闲段，则同时查看 CPU 提交、同步和 F3+2 分项，避免把CPU等待都算作shader成本。

## 需要源码和逐调用信息时

第一轮确定热点后，再单独开启 **Real-Time Shader Profiler**，采集同样的画面；需要更多硬件指标时再使用 Multi-Pass Metrics。它们与基础采集分别保存并注明选项。Shader Profiler 和采集器自身会消耗资源，逐调用强制计时也会扰动性能，不把这些模式下的数字混作正常运行基线。

默认 `release-lto` 用于发行性能对照，会省略 Slang 调试信息。需要源码关联时，可在 Prime PT 根目录另建保留着色器调试信息的优化构建：

```powershell
./gradlew.bat --offline -PnativeProfile=release buildNative
if ($LASTEXITCODE -ne 0) { throw '诊断构建失败' }
$nativeDll = (Resolve-Path './target/release/prime_pt.dll').Path
$launchArgs = '/d /s /c ""{0}" --offline --no-daemon runClient "-PnativeLibrary={1}""' -f $gradleBat, $nativeDll
```

重新建立输出目录，再用前面的 Nsight 命令启动。`-PnativeLibrary` 决定实际加载的DLL；修改构建档位后也必须更新该路径。保留对应源码、DLL、调试文件和shader缓存；实际可关联的源码范围以报告为准。此构建与 `release-lto` 分组比较，不混入同一性能样本。

若需要查看资源、光追管线和某个调用的输出，可另选 **Graphics Capture** 抓单帧，再在 Graphics Debugger 中回放。GPU Trace 与 Graphics Capture 的结果文件及用途不同；扩展或回放不受当前工具支持时，保留错误并先分析可取得的 GPU Trace，不将回放失败记作应用渲染通过。[Nsight Graphics 2026.1 功能说明](https://developer.nvidia.com/nsight-graphics/getting-started/release-note-v2026.1)

## 留下可对照的记录

每次使用新的输出目录，保留原始 `.ngfx-gputrace`。采集结束、日志写入稳定后，在准备这次启动的 PowerShell 中保存：

```powershell
Copy-Item -LiteralPath './run/config/prime_pt.properties' -Destination (Join-Path $captureDir 'settings.properties')
Copy-Item -LiteralPath './run/logs/latest.log' -Destination (Join-Path $captureDir 'latest.log')
Copy-Item -LiteralPath $nativeDll -Destination (Join-Path $captureDir 'prime_pt.dll')
Get-FileHash -Algorithm SHA256 -LiteralPath $nativeDll |
    Format-List | Out-File (Join-Path $captureDir 'native-sha256.txt') -Encoding utf8
git rev-parse HEAD | Out-File (Join-Path $captureDir 'prime-head.txt') -Encoding utf8
git diff --binary --output="$captureDir/prime.patch"
git -C ../.. rev-parse HEAD | Out-File (Join-Path $captureDir 'engine-head.txt') -Encoding utf8
git -C ../.. diff --binary --output="$captureDir/engine.patch"
```

有未跟踪的新源码时也一并保存，`git diff` 不包含它们。另记窗口与实际输入分辨率、GPU与驱动、Nsight版本、构建档位、世界位置、SER/RR实际状态、M上限、fresh比例、采集选项和操作。不要只保留截图；时间轴截图用于交流，原始文件用于重新分析。

需要批量读取已有trace时，参见引擎的 [GPU Trace离线读取手册](../../../manual/nsight.md)。从引擎根目录运行其脚本，输出使用另一个新目录；工具标为partial时保留缺失原因。

## 常见失败

| 现象 | 检查方法 |
| --- | --- |
| 只有Gradle进程，没有可连接的游戏进程 | 确认从Nsight启动、参数含`--no-daemon`，查看终端及`run/logs/latest.log`；构建失败先解决，不反复尝试Attach。 |
| F11切换了全屏、分辨率或抓到了菜单 | 重新绑定Nsight快捷键，恢复固定窗口，回到世界预热后重抓。 |
| 提示无权访问GPU性能计数器 | 按NVIDIA的[性能计数器权限说明](https://developer.nvidia.com/ERR_NVGPUCTRPERM)配置；不能读取的指标记为不可用。 |
| 找不到Slang、SDK或Java | 回到[环境准备](build.md#准备源码环境)，从准备好环境的终端启动Nsight；确认`buildNative`已成功。 |
| 注入、Vulkan扩展或Graphics Capture回放失败 | 保存Nsight错误、游戏日志及工具/驱动版本；先区分连接失败与回放失败，不用空报告推断耗时。 |
| 抓帧后明显变慢或PCIe流量很高 | 对照关闭Shader Profiler、多轮指标与逐调用计时的基础采集，保持时钟和场景一致；采集开销也可能影响结果。 |
| 运行日志有验证错误 | 原样保留并单独处理；关闭验证后的计时不能证明原来的同步问题已修复。 |
