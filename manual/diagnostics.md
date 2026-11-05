# 诊断、测试与性能测量

遇到启动或游玩问题，可以先看[安装后的检查步骤](build.md#安装发行包)和下方的[游戏诊断](#在游戏中读取诊断)。开发检查和性能测量的命令在 **PowerShell 7** 中运行，当前目录应是含有 `gradlew.bat` 的 Prime PT 根目录。工具链和设备要求见[构建手册](build.md)。

需要检查实际游戏的光追、ReSTIR或RR成本时，按 [Nsight抓帧手册](nsight.md) 从工具启动客户端，先抓普通PT与ReSTIR的原生无累积对照，再展开热点。

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
cargo test --locked -p prime_renderer --lib hardware_pt_restir_ -- --ignored --nocapture --test-threads=1
.\scripts\test-streamline.ps1 -InitializationOnly
.\scripts\test-streamline.ps1
```

遇到 RR 回退时先检查设备实际启用的能力、SDK 初始化与每帧求值日志，再查看引擎 [Streamline 手册](../../../manual/streamline.md) 的同步验证说明。关闭同步验证所得结果按普通验证记录。

## 在游戏中读取诊断

使用 **F3+2** 打开 Prime 分项图。左侧显示 Prime 的帧耗时，右侧保留 Minecraft 的 TPS 图。先检查最大的分项、预算压力和近期场景更新，再结合移动、编辑或资源重载的时间点读日志。分项显示主线程实际经过的时间，单位是毫秒。耗时越长，画面更新通常越慢。场景更新摘要可能来自与所选历史帧不同的更新。字段归属见[诊断模块文档](../target/doc/prime_pt/diagnostics/index.html)。

手工验收可依次检查进出世界、跨区移动、放置和破坏方块、草叶水色、镂空与玻璃、局部发光、预算压力、资源重载、SER／RR／画质切换、窗口缩放与退出。记录原版客户端版本、显卡驱动、画质与视距，并将游戏结果与编译、CPU 和离屏结果分开报告。

## 时间历史的显式管理

普通PT的静态图像累积、RR重建与ReSTIR路径复用是三个不同消费者。失效由同一个事件入口管理，不在参数查询或描述符重绑时暗中清零。

| 事件 | 影响范围 |
| --- | --- |
| 切换世界、模型/材质身份域整体替换、切换渲染模式/积分器、显式请求全局失效 | 丢弃三个消费者的历史，保留曝光适应；绝对RR样本与jitter序号不回退。 |
| 相机/FOV、anchor、日照、介质、seed/路径预算、区块/属性变化 | 普通PT静态均值显式重新开始；RR和ReSTIR继续，失效mesh按稳定身份局部拒绝。 |
| RR、ReSTIR或诊断模式的逐帧图像输出 | 每帧显式重开RGB样本域，保留时间复用历史。 |
| SER、M cap、fresh输出比例变化 | 保留路径工作集与时间历史。 |
| 实际视图或SDK存储替换 | 分别记录冷启动；ReSTIR只有输入尺寸改变才更换其工作集，同尺寸重绑保留历史。 |
| RR求值失败 | 显式失效RR，保留ReSTIR；未提交帧的取消不清除已接受路径或相机。 |

在游戏日志中搜索`pt.history`、`rr.history`和`restir.history`。`action=reset`表示封闭全局事件，`storage_cold`表示实际存储冷启动，`recovery_reset`表示RR失败恢复；`valid`记录此前是否存在已接受历史。每帧RGB样本域与静态均值的局部失效只计数，不逐帧打印日志。代码契约从[GPU模块文档](../target/doc/prime_renderer/gpu/index.html)查看。

ReSTIR保留了缓存后缀的入射辐射，不保证任意动态光照下没有鬼影或完全无偏；M cap约束样本量，不是历史存活帧数。离线样本到达精确累积上限会报错，需显式重新进入离线模式，不自动清空图像。

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

比较两种渲染器的固定场景成本时，需要支持 EXT SER 的设备，先关闭其他 GPU 程序，再运行：

```powershell
New-Item -ItemType Directory -Force temp | Out-Null
$env:PRIME_PT_RENDER_BENCH_OUTPUT = Join-Path (Get-Location).Path "temp/render-$((Get-Date).ToString('yyyyMMdd-HHmmss')).json"
cargo test --locked --release -p prime_renderer --lib hardware_pt_render_profile -- --ignored --nocapture --test-threads=1
```

JSON 保存每帧 CPU 录制时间和 GPU 时间戳跨度。输入是 960×540、4096 个四边形、每帧一个样本和12顶点预算；不包含 RR、Java 捕获、场景更新和窗口呈现，不能换算为 Minecraft 帧率。编译后首次运行还会建立驱动缓存，应保留记录并单独重跑校准。
