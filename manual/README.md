# 操作手册

如果只想游玩，先看[安装发行包](build.md#安装发行包)。要从源码构建时，再准备开发工具并按[目录检查](build.md#准备源码环境)打开 PowerShell。

- [安装、构建与开发运行](build.md)
- [视频设置与本地命令](settings.md)
- [诊断、测试与性能测量](diagnostics.md)
- [用 Nsight 抓取 GPU 帧](nsight.md)

## 生成文档

Rustdoc 是从源码注释生成的网页。先按[构建手册](build.md#准备源码环境)准备 Slang 和 Streamline SDK，再在同一个 PowerShell 窗口、Prime PT 根目录生成文档。

```powershell
cargo doc --locked --workspace --no-deps --document-private-items
```

看到 `Generated` 后，打开本机的 [Prime PT Rust 文档](../target/doc/prime_pt/index.html)。组件关系在原生入口的 crate 文档，数据契约和生命周期在相应 crate、模块、类型与函数的文档注释。命令中的 `--document-private-items` 同时生成内部模块文档。

开发约定见 [AGENTS.md](../AGENTS.md)。`target/` 和 `build/` 保存可再生产物，`temp/` 保存原始诊断与测量。
