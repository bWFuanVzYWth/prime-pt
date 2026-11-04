# 诊断、测试与性能测量

遇到启动或游玩问题，可以先看[安装后的检查步骤](build.md#安装发行包)和下方的[游戏诊断](#在游戏中读取诊断)。开发检查和性能测量的命令在 **PowerShell 7** 中运行，当前目录应是含有 `gradlew.bat` 的 Prime PT 根目录。工具链和设备要求见[构建手册](build.md)。

## 开发检查

```powershell
cargo test --locked --workspace --lib
.\gradlew.bat check
.\gradlew.bat compactSourceFixture modelRandomFixture tintFixture
cargo test --locked -p prime_minecraft --lib -- --include-ignored
```

夹具是为测试准备的一组固定输入。Java 夹具生成的文件位于 `target/`，对应的 Rust 测试会读取它们。Gradle 成功时显示 `BUILD SUCCESSFUL`，Rust 成功时显示 `test result: ok`。缺输入时先运行上面的 Gradle 夹具命令。

## GPU 与 Streamline

离屏 GPU 测试按名称单独运行。两个 `hardware_pt_rr_` 用例必须分别启动独立进程。

```powershell
cargo test --locked -p prime_renderer --lib hardware_pt_material_reload -- --ignored --nocapture --test-threads=1
.\scripts\test-streamline.ps1 -InitializationOnly
.\scripts\test-streamline.ps1
```

遇到 RR 回退时先检查设备实际启用的能力、SDK 初始化与每帧求值日志，再查看引擎 [Streamline 手册](../../../manual/streamline.md) 的同步验证说明。关闭同步验证所得结果按普通验证记录。

## 在游戏中读取诊断

使用 **F3+2** 打开 Prime 分项图。左侧显示 Prime 的帧耗时，右侧保留 Minecraft 的 TPS 图。先检查最大的分项、预算压力和近期场景更新，再结合移动、编辑或资源重载的时间点读日志。分项显示主线程实际经过的时间，单位是毫秒。耗时越长，画面更新通常越慢。场景更新摘要可能来自与所选历史帧不同的更新。字段归属见[诊断模块文档](../target/doc/prime_pt/diagnostics/index.html)。

手工验收可依次检查进出世界、跨区移动、放置和破坏方块、草叶水色、镂空与玻璃、局部发光、预算压力、资源重载、SER／RR／画质切换、窗口缩放与退出。记录原版客户端版本、显卡驱动、画质与视距，并将游戏结果与编译、CPU 和离屏结果分开报告。

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
