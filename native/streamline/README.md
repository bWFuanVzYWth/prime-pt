# Prime PT Streamline 接入

通用 bridge、SDK 锁、构建与独立进程 GPU 验证已移到引擎
[`crates/vulkan/streamline`](../../../../crates/vulkan/streamline/README.md)。
本 mod 的 `native/src/streamline.rs` 只提供 Prime PT 应用身份与接口转发。
SDK 缓存仍保留于 `third_party/streamline`，正式构建通过显式 `STREAMLINE_SDK` 读取
引擎安装器生成的独立 SDK 根；不编译本目录，也不读取旧项目。
