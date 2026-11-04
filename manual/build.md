# 安装、构建与开发运行

已经有 `prime-pt-0.1.0.jar`，从「安装发行包」开始。手上只有源码，先走「准备源码环境」和「构建发行包」。

## 安装发行包

1. 在启动器中创建或选择 **Minecraft Java 版 26.3** 的游戏实例。实例就是一份独立的游戏目录和启动配置。为它选择 **Fabric Loader 0.19.5 或更新版本**及 **Java 25**。可从 Fabric 官方页面按所用启动器安装。[Fabric 安装入口](https://fabricmc.net/use/installer/)
2. 在启动器里找到这个实例使用的游戏目录。打开其中的 `mods/`，如果没有该目录就新建它。把 `prime-pt-0.1.0.jar` 原样放进去，保留 `.jar` 扩展名。
3. 在实例的**游戏参数**里加上 `--graphicsBackend VULKAN`。这是传给 Minecraft 的参数，请放在启动器提供的游戏参数位置。
4. 启动这个实例，打开视频设置，检查是否出现 Prime PT 的设置项。进入一个世界即可观察方块场景。首次启动会在游戏目录下的 `.primept/runtime/` 校验并提取运行时，发行包已自带所需的原生库、Slang、着色器及 Fabric 资源加载模块。

启动失败时，先核对实例的游戏版本、Java 版本、Fabric Loader 和游戏参数，再在该游戏目录的 `logs/latest.log` 搜索 `prime_pt` 或 `Vulkan`。如果只是 RR 未启用，检查[设备要求](#检查显卡与驱动)和[诊断步骤](diagnostics.md#gpu-与-streamline)。

## 检查显卡与驱动

先确认系统为 **Windows x64**，并安装显卡厂商提供的兼容驱动。当前发行构建以 `x86-64-v3` 为处理器基线，其中包含 AVX2 等指令。

Vulkan 是游戏调用显卡的接口。Prime PT 需要 Vulkan 1.4 loader 和实际支持 Vulkan 1.4 的显卡及驱动，基础路径追踪还要求以下能力：

- 光追管线、用于组织场景几何的加速结构、RayQuery（着色器内查询光线与几何的交点）。
- `VK_KHR_ray_tracing_position_fetch` 和 `VK_KHR_device_address_commands`。
- buffer device address、synchronization2 和 extended storage image formats。

可查驱动的 Vulkan 能力报告；已有 `vulkaninfo` 工具时，可用 `vulkaninfo --summary` 查看实际设备及 API 版本，用 `vulkaninfo` 的完整输出搜索扩展名。使用多显卡时以游戏实际选择的设备和启动日志为准。DLSS Ray Reconstruction 另需支持该功能的 NVIDIA 显卡和驱动，属于可选画质功能。

## 准备源码环境

Prime PT 是独立项目，构建时从上两级目录读取 simple_engine 源码。目录布局应是 `simple_engine/app/prime_pt`。在 **PowerShell 7** 中切换到 Prime PT 根目录，也就是能看到 `gradlew.bat` 的那一层。

```powershell
Set-Location 'D:/projects/simple_engine/app/prime_pt'
Test-Path ./gradlew.bat
Test-Path ../../crates/vulkan/Cargo.toml
```

两条都应显示 `True`。第二条为 `False` 时先确认引擎和 Mod 的相对位置。

安装 **Java 25 JDK**（含 Java 编译器）、**rustup**（用于选择仓库固定的 Rust 工具链）及 **MSVC x64 C++ 构建工具和 Windows SDK**。Gradle 是 Java 项目的构建工具，本项目用 `gradlew.bat` 选择所需版本。首次运行会下载构建工具和依赖。工具可从官方页面获取。[Java 25 JDK](https://adoptium.net/temurin/releases/?version=25)、[Rust 安装](https://rust-lang.org/tools/install/)、[微软 C++ 构建工具](https://visualstudio.microsoft.com/visual-cpp-build-tools/)、[PowerShell 7 安装](https://learn.microsoft.com/en-us/powershell/scripting/install/install-powershell-on-windows)

```powershell
java -version
javac -version
cargo --version
./gradlew.bat --version
```

Java 和 javac 应显示 25，Cargo 与 Gradle 应能打印版本。找不到 javac 时检查当前终端选择的是 JDK，找不到 Cargo 时检查 rustup 安装。环境修改后重新打开终端再试。

安装 Streamline SDK，这是构建中使用的 NVIDIA 重建接口开发包。

```powershell
./scripts/setup-streamline.ps1
```

成功时会显示 SDK 校验结果和 `STREAMLINE_SDK` 的位置。安装器默认使用引擎的 `target/streamline-sdk`，已有 SDK 可加 `-VerifyOnly`。后续构建在同一终端执行。校验失败时看[引擎 Streamline 手册](../../../manual/streamline.md)。

## 构建发行包

```powershell
./scripts/build-release.ps1
```

脚本会准备 Slang（用于编译 GPU 程序），也可以通过 `SLANGC` 指定已有发行包中的编译器。看到 `BUILD SUCCESSFUL` 后，在 `build/libs/` 找到 `prime-pt-0.1.0.jar`，再按[安装发行包](#安装发行包)操作。打包过程会检查原生库路径，并从成品 JAR 验证解包、原生库加载和运行时着色器编译。

离线构建可加 `-Offline`，需事先准备 Gradle、Cargo 和 Slang 缓存。找不到 MSVC CRT（C++ 运行库）时，可通过 Gradle 的 `-PvcRedistDir` 指向 x64 可再发行目录。默认的 `release-lto` 是带优化且省略着色器调试信息的发行配置。

## 运行开发客户端

开发客户端使用项目内的 `run/` 作为游戏目录。确认前面的工具和 SDK 都已准备。直接运行开发客户端时，可先用 `../../scripts/install_slang.ps1` 安装引擎固定的 Slang，然后留在同一 PowerShell 窗口执行：

```powershell
./gradlew.bat runClient
```

成功时会打开 Minecraft 客户端。开发运行的日志可在 `run/logs/` 查看。常用调试选项如下。

- `-Prr=false` 跳过早期 SDK 接入，以原始路径追踪启动。之后启用 RR 需要重启。
- `-Pser=false` 为没有持久化设置的配置提供关闭 SER 的默认值。SER 是设备可以选择的着色器执行重排功能。
- `-PnativeProfile=release` 选择保留 Slang 调试信息的诊断构建。
- `-PnativeLibrary=绝对路径` 可在 `runClient` 和 native 夹具中复用现有 DLL。其同目录应有 SDK 运行库与许可。开发 DLL 通过工作目录的祖先定位项目源码。

开发的 shader 和驱动管线缓存位于 `target/shaders` 与 `target/pipeline-cache`，发行版在提取目录的 `cache/` 下建立对应缓存。设备或驱动变化后，以日志中的缓存加载结果确认实际复用情况。
