# 诊断、测试与性能测量

遇到启动或游玩问题，可以先看[安装后的检查步骤](build.md#安装发行包)和下方的[游戏诊断](#在游戏中读取诊断)。开发检查和性能测量的命令在 **PowerShell 7** 中运行，当前目录应是含有 `gradlew.bat` 的 Prime PT 根目录。工具链和设备要求见[构建手册](build.md)。

需要检查实际游戏的光追或RR成本时，按 [Nsight抓帧手册](nsight.md) 从工具启动客户端，先抓PT的原生与RR对照，再展开热点。

## 捕获可复现的世界画面

场景快照保存已准备的真实网格、材质贴图、发光采样表、相机和照明，供不启动游戏的 GPU 回放使用。
它是同版本开发诊断数据，不是 Minecraft 存档，也不包含服务端模拟。

先关闭开发客户端。在已配置源码开发环境的 **PowerShell 7** 中，从 Prime PT 目录执行：

```powershell
Set-Location 'C:\repositories\simple_engine\app\prime_pt'
.\scripts\capture-scene.ps1 -WorldPath '.\run\saves\GNLXC_main_v1.0.1' -OutputDirectory 'C:\repositories\simple_engine\temp\world-capture-01'
```

输出目录必须是新目录。脚本复制地图和设置，只运行副本；窗口使用 4K，RR 档位设为 DLAA。
进入保存视角后不要移动，默认经过 600 个有效渲染帧写入一次。可用 `-Frames 1200` 增加等待，
也可用 `-PrepareOnly` 只复制输入和生成 `launch.json`，不启动游戏。
出现 `scene_capture complete`，且输出目录的 `capture\complete` 存在，表示快照写入完成。
检查原生输出，而不是仅检查 `latest.log`；资源、编译或磁盘错误不会生成完成标记。
确认地图已加载到需要的范围，再退出这个副本。

无窗口回放的输入固定为该快照。继续在同一 PowerShell 中执行：

```powershell
$env:PRIME_PT_REPLAY = 'C:\repositories\simple_engine\temp\world-capture-01\capture'
$env:PRIME_PT_REPLAY_FRAMES = '128'
cargo test -p prime_renderer --release --lib hardware_world_capture_replay -- --ignored --nocapture --test-threads=1
```

默认按 4K 回放普通 PT，关闭 RR 和跨帧 RGB 累积。结果写入 `capture\replay`，包括每帧 HDR 最大值、极值像素位置、均值和高亮像素计数。
`*-peak.rgba32fle` 和 `*-last.rgba32fle` 是小端 RGBA32F 原始数据，尺寸见同名 JSON；不是可直接打开的图片。
需要重复运行时，将 `$env:PRIME_PT_REPLAY_OUTPUT` 设成另一个新子目录名。此过程包含大量读回和磁盘写入，不用于性能比较。
SER 默认沿用快照开关；用 `$env:PRIME_PT_REPLAY_SER = '0'` 或 `'1'` 做独立对照。
水下和玻璃后的稀有焦散可能长时间不收敛；短序列均值接近不能证明积分器正确。
## PT 等时间质量与 RR 对照

无窗口入口使用生产材质与光传输程序，比较实时首表面策略和离线批量采样，不改变游戏设置。
先按[构建手册](build.md)准备 Rust、Slang、Streamline SDK 和支持 SER 的 NVIDIA 显卡，关闭其他 GPU 程序。
在 **PowerShell 7** 中，从 Prime PT 根目录执行；输出目录必须尚不存在：

```powershell
Set-Location 'C:\repositories\simple_engine\app\prime_pt'
New-Item -ItemType Directory -Force 'C:\repositories\simple_engine\temp' | Out-Null
$env:PT_QUALITY_CONFIG = 'C:\repositories\simple_engine\temp\pt-quality.json'
@'
{
  "output": "C:\\repositories\\simple_engine\\temp\\pt-quality-01",
  "scene": "occluded",
  "width": 3840, "height": 2160,
  "warmup": 20, "frames": 2048, "seeds": [17, 4106],
  "gpu_checkpoints_ms": [250, 1000, 4000],
  "variants": [
    {"name": "pt2", "kind": "pt", "spp": 2},
    {"name": "offline4", "kind": "offline", "spp": 4}
  ]
}
'@ | Set-Content -Encoding utf8 $env:PT_QUALITY_CONFIG
cargo test -p prime_renderer --release --lib hardware_pt_equal_time_benchmark -- --ignored --nocapture --test-threads=1
```

成功时测试返回 `ok`，输出根目录有 `report.json`。每个变体和种子保留输入与原始记录；
`mean-time-4000.f32` 是累计 GPU 时间首次达到 4 秒后的 HDR 算术均值，实际时间、帧数和尺寸见报告。
文件是小端 RGBA32F 原始数据，不能直接当作图片打开。
GPU 时间包括首视线、导引、照明、后处理及启用时的 RR；准备、测量用 GPU 累积、读回和写盘不计入。
先做同版本 A/A 校准，再比较实际时间相近的结果；相同 spp 或帧数不等于相同成本。

常用输入：

- `scene`：`direct` 为直接照明室内，`occluded` 的灯光受挡板遮挡，`glossy` 另有倾斜金属面。
- `kind`：`pt` 使用实时质量策略，固定 2 spp；`offline` 使用原材质概率，批量为 1–64。
- `rr: true` 固定原生 4K + DLAA。比较重建时删去 `offline4` 变体，保留正常镜头抖动与 RR 历史；输出 `rr-time-预算.f16` 为 RGBA16F。
- `motion: true` 先静止 32 帧，再横移 32 帧，随后静止。此时删除 GPU 预算并设置固定帧数，例如 `frames: 96`；运动序列的跨帧均值不用于收敛判断。
- `rr_frames`、`raw_frames` 指定额外保存图像的帧。默认保留末帧；`save_raw_final: false` 可省略未重建末帧。
- `center_film` 在无 RR 时默认 `true`，固定像素中心以隔离采样误差；有 RR 时默认 `false`。制作 RR 参考图时使用 `offline` 并设为 `false`，覆盖像素内采样。
- `metric_reference` 可指定同尺寸 RGBA32F 独立高采样参考图，配合 GPU 预算报告逐像素和不同区域尺度的误差。参考噪声不会被自动扣除，也不裁剪亮点。RR 模式不支持此项或 `save_means`。

没有 GPU 预算时按固定帧数运行；有预算时 `frames` 是上限，未达到预算会返回错误。
`frames-block16.f32` 保存逐帧 16×16 区域的 RGB 均值和极值，用于检查空间聚集与时间相关。
大量筛选可设置 `save_means: false`、`save_raw_final: false`、`save_block_frames: false` 限制输出。
失败时保留完整日志；已知 NGX 内部同步错误仍使进程失败，不能记为验证通过。

## F11 和窗口卡死

上述捕获脚本及 `run-gpu-diagnostic.ps1` 同时记录窗口边界。
搜索 `window_transition`，保留 `setMode`、`SDL_SetWindowFullscreen`、`SDL_SyncWindow` 和 `swapchain` 的 `BEGIN`／`END`。
缺少 `END` 能缩小阻塞范围，但不能仅凭它判定驱动或 SDK 有错。
卡住时先保留线程栈，再关闭进程。在另一个 PowerShell 中运行以下命令，将 `12345` 替换为该客户端的进程 ID：

```powershell
Get-Process java,javaw -ErrorAction SilentlyContinue | Select-Object Id,MainWindowTitle
& "$env:JAVA_HOME\bin\jcmd.exe" 12345 Thread.print > 'C:\repositories\simple_engine\temp\f11-threads.txt'
```

同时记录独占／无边框模式、退出前后的分辨率及 RR 档位。不要把仍在运行的 Gradle 后台进程误认成客户端。

## 开发检查

```powershell
cargo test --locked --workspace --lib
.\gradlew.bat check
.\gradlew.bat compactSourceFixture modelRandomFixture tintFixture
cargo test --locked -p prime_minecraft --lib -- --include-ignored
```

夹具是为测试准备的一组固定输入。Java 夹具生成的文件位于 `target/`，对应的 Rust 测试会读取它们。Gradle 成功时显示 `BUILD SUCCESSFUL`，Rust 成功时显示 `test result: ok`。缺输入时先运行上面的 Gradle 夹具命令。

## GPU 与 Streamline

离屏 GPU 测试按名称单独运行。`hardware_pt_rr_` 用例必须分别启动独立进程。

```powershell
cargo test --locked -p prime_renderer --lib hardware_pt_material_reload -- --ignored --nocapture --test-threads=1
.\scripts\test-streamline.ps1 -InitializationOnly
.\scripts\test-streamline.ps1
```

遇到 RR 回退时先检查设备实际启用的能力、SDK 初始化与每帧求值日志，再查看引擎 [Streamline 手册](../../../manual/streamline.md) 的同步验证说明。关闭同步验证所得结果按普通验证记录。

## 运行一段时间后出现 DEVICE_LOST

`DEVICE_LOST` 表示 GPU 设备无法继续工作；“等待提交完成失败”本身无法指出哪次计算出错。
优先使用下面的异步检查点模式。它保留原提交顺序，在光追、计算派发和 RR 前后插入驱动进度标记，
不会逐阶段等待 GPU。出现设备丢失后，日志记录驱动最后经过的标记及近期录制信息。
需要设备支持 `VK_NV_device_diagnostic_checkpoints`；扩展不可用时会明确记录，不能把缺少检查点当作 GPU 没有执行。

先关闭已有开发客户端。在 **PowerShell 7** 中复制执行；这里使用已配置好的源码开发环境：

```powershell
Set-Location 'C:\repositories\simple_engine\app\prime_pt'
.\scripts\setup-streamline.ps1 -VerifyOnly
.\scripts\prepare-nsight.ps1 -Offline
.\scripts\run-gpu-diagnostic.ps1 -Offline -Mode checkpoints
```

最后一个脚本会启动游戏，前两个只检查工具和构建。进入原来发生问题的世界，保持相同的 RR、SER 和分辨率设置，
按原来的方式游玩或移动。不要开启 `/pt diagnostic on`，它是另一项关闭 RR 的图像诊断功能。
已有本次源码构建出的 `target/release/prime_pt.dll` 时只需最后一条启动命令。
缺少离线依赖时去掉 `-Offline`；只想检查启动输入而不打开游戏时加 `-PrepareOnly`。

终端会显示引擎根目录下的新目录 `temp/prime-gpu-日期时间-checkpoints`。
`native.log` 同时保存 Java 与原生库的输出；原生检查点不保证出现在 Minecraft 的 `latest.log` 中。
成功进入渲染后应能找到 `gpu_checkpoints enabled`，设备丢失后搜索 `gpu_fault`。
退出或崩溃后保留整个目录，其中还包含实际启动的 DLL／SDK 副本、源码差异、设置、启动参数和本次崩溃报告。
不需要先用 Nsight 抓帧；普通运行即可。此诊断模式仍可能带来额外开销，不用于性能验收。

| 最后日志 | 含义 |
| --- | --- |
| `gpu_fault stage=0x1` | 驱动在 TOP_OF_PIPE 阶段最后经过的标记；不能据此认为操作已完成。 |
| `gpu_fault stage=0x2000` | 驱动在 BOTTOM_OF_PIPE 阶段最后经过的标记；用于缩小未完成区间。 |
| 其他 `stage` | Vulkan 阶段原始位；保留原值交给分析，不猜测成某个 shader。 |
| `checkpoint=Some(...)` | `scope` 是帧样本序号，`label` 是边界，`pipeline` 和 `extent` 对应具体派发。 |
| `checkpoint=None` | 驱动标记不在保留的最近 512 条记录中，不能解释为没有执行。 |
| `recorded_only` | 只代表 CPU 录制过，不能当作 GPU 进度。 |
| `cpu_only` | 最近场景发布、更新帧号、内存预算压力及捕获／编译工作量。CPU 可能已准备下一帧，不能据此断定某次更新导致了 GPU 故障。 |

驱动检查点用于缩小范围，不证明对应命令就是崩溃原因，也不恢复设备。
如果先出现地形镂空、纹理消失或区块缺失，先按 F2 保存异常画面，并记下是否静止、是否自行恢复；
随后保留原设置继续观察。截图和故障目录一起保存。仅凭剩余轮廓，无法区分错误透明度、缺失几何和历史重建异常。
如果需要进一步定位，可以改用逐阶段等待：它只在 RR 实际启用时对 RR 之前的自有操作独立提交并等待，
日志中的 `state=complete` 才表示对应操作已确认完成。RR 和呈现仍按原顺序交给宿主。
这种模式会改变时序，可能使竞态暂时消失，不能用于性能比较。

```powershell
.\scripts\run-gpu-diagnostic.ps1 -Offline -Mode stages
```

完全关闭两种诊断时使用下面的命令。所有诊断环境变量只对脚本启动的进程生效，结束后恢复终端原值，不修改游戏设置。

```powershell
.\scripts\run-gpu-diagnostic.ps1 -Offline -Mode normal
```

## 在游戏中读取诊断

右上角持续显示“Prime PT 不可用”时，游戏保留原版渲染，不会初始化 Prime 的光追资源。
提示 OpenGL 时，在原版视频设置中选择 Vulkan 后重启游戏；提示缺少设备能力时，查看
`run/logs/latest.log` 中“缺少”后的完整扩展和特性列表，再核对[硬件要求](build.md#检查显卡与驱动)。
通知使用原版系统提示样式，不会自动超时消失；修改 Prime 开关不会伪装成设备已经兼容。

使用 **F3+2** 打开 Prime 分项图。左侧显示 Prime 的帧耗时，右侧保留 Minecraft 的 TPS 图。先检查最大的分项、预算压力和近期场景更新，再结合移动、编辑或资源重载的时间点读日志。分项显示主线程实际经过的时间，单位是毫秒。耗时越长，画面更新通常越慢。场景更新摘要可能来自与所选历史帧不同的更新。字段归属见[诊断模块文档](../target/doc/prime_pt/diagnostics/index.html)。

手工验收可依次检查进出世界、跨区移动、放置和破坏方块、草叶水色、镂空与玻璃、局部发光、预算压力、资源重载、SER／RR／画质切换、窗口缩放与退出。记录原版客户端版本、显卡驱动、画质与视距，并将游戏结果与编译、CPU 和离屏结果分开报告。

## 时间历史的显式管理

静态图像累积与 RR 重建分别维护历史，由同一个事件入口显式失效；参数查询或描述符重绑不暗中清零。

| 事件 | 影响范围 |
| --- | --- |
| 切换世界、资源身份域替换、切换实时／离线或显式全局失效 | 丢弃累积和 RR 历史，保留曝光适应；随机与 jitter 序号不回退。 |
| 相机、FOV、anchor、日照、介质、路径预算或区块变化 | 静态均值重新开始；RR 使用当前导引继续重建。 |
| RR 或诊断模式的逐帧输出 | 每帧重开 RGB 样本域，保留 RR 历史。 |
| SER 切换 | 保留时间历史。 |
| 实际视图或 SDK 存储替换 | 单独记录存储冷启动。 |
| RR 求值失败 | 显式失效 RR；取消未提交帧不推进已接受相机和样本身份。 |

日志中的 `pt.history` 和 `rr.history` 记录显式事件。`action=reset` 表示失效，
`storage_cold` 表示存储冷启动，`recovery_reset` 表示 RR 失败恢复；`valid` 记录此前是否存在已接受历史。
逐帧 RGB 样本域和静态均值局部失效只计数，不逐帧打印。契约见[GPU 模块](../target/doc/prime_renderer/gpu/index.html)。
离线样本到达精确累积上限会报错，需重新进入离线模式，不自动清空图像。

## 性能测量

CPU 基准无需启动游戏或占用 GPU。

```powershell
New-Item -ItemType Directory -Force temp | Out-Null
$env:PRIME_WORLD_BENCH_OUTPUT = Join-Path (Get-Location).Path "temp/prime-source-$((Get-Date).ToString('yyyyMMdd-HHmmss'))"
cargo bench --locked -p prime_world --features test-fixtures --bench source_pipeline
```

场景基准需要独占硬件光追设备。每次使用 `temp/scene-bench/` 下的新目录。

```powershell
$env:PRIME_PT_SCENE_BENCH_OUTPUT = "run-$((Get-Date).ToString('yyyyMMdd-HHmmss'))"
cargo test --locked --release -p prime_renderer --lib hardware_pt_scene_baseline -- --ignored --nocapture --test-threads=1
```

保留 `samples.jsonl`、输入摘要和构建身份。先冻结原始版本做同版本 A/A，再在同一环境对比成对样本。计时范围和排除项见 [CPU 基准注释](../crates/world/benches/source_pipeline.rs) 与 [GPU 场景基准注释](../crates/renderer/src/scene_bench.rs)。

## 检查水平转向与远处方块的体积光

准备好 Vulkan 光追设备后，在 Prime PT 根目录的 PowerShell 中运行无窗口测试。

```powershell
cargo test --release --locked -p prime_renderer --lib terrain_volume -- --ignored --nocapture --test-threads=1
```

转向用例比较转向首帧与在同一视角重建的画面。远景用例确认主射线命中一米宽方块，再把方块表面的空气散射与同深度的大平面比较。输出还包含原生 3840×2160 连续转向的整帧 GPU 耗时。计时场景有近墙和远墙，预热 8 帧后报告 17 帧的中位数。出现断言失败时，可按输出中的用例名称单独重跑。

## 对照泛光模式下的点光源与太阳盘

准备好[构建工具](build.md)和 Vulkan 光追设备后，在 Prime PT 根目录打开 PowerShell。无窗口测试用相同曝光将高亮白点和蓝天中的太阳盘送入实际呈现管线，分别检查相机光圈、瞳孔和快速圆形的点扩散与太阳周围的亮度。

```powershell
$env:VK_LAYER_PATH = Join-Path $env:VULKAN_SDK Bin
$env:PRIME_PT_DIFFRACTION_OUTPUT = Join-Path (Get-Location) temp/point-source
cargo test --release --locked -p prime_renderer hardware_pt_aperture_ -- --ignored --nocapture --test-threads=1
```

成功时七项测试通过。点光源用例报告 RGB 总通量，两个 FFT 镜头另报告相对导出核的最大逐像素绝对误差。太阳用例给出横、纵两个方向比天空亮 1% 的半径，单位为输出像素。切换用例覆盖更换光瞳、关闭泛光和恢复原选择，同时核对图像、累积历史与显存回收。

在输出目录中并排打开 `camera-sun-display.png`、`pupil-sun-display.png` 和 `fast-sun-display.png`。同一前缀下，`*-point-display.png` 显示单像素白点，`*-linear.rgba32f` 保留对应的线性 HDR 回读，`*-point.json` 记录输入位置和通量。验证层或设备初始化失败时，先检查 Vulkan SDK 和显卡驱动。
