# Prime PT

Minecraft 26.3 / Fabric 的 Prime PT 重写，参考旧 Prime PT 的代码与契约。此目录是独立 Git 仓库和 Cargo workspace，依赖上两级的 simple_engine；引擎不依赖本目录。`target/`、`run/` 和 SDK 缓存不纳入版本管理。

Java 每帧薄捕获一个 4×4×4 section region（64³ 方块）：转发原版模型的 quad、atlas UV、面可见性和群系 tint。Rust 在批次层调度 CPU 工作池，缓存 section、合批直接寻址 quad，并仅为 AS 构建展开三角形；引擎 `vulkan::native` 拥有 GPU 资源和 BLAS/TLAS，`pt::gpu::Renderer` 录制 K1→K2→K3。使用 RT Pipeline、叶子材质 callable 与 SBT；Slang 直接生成 SPIR-V。

与 game 共用 `shaders/pt/{integrator,path_sampler,post}.slang`，包含 OpenPBR、Beer、NEE/MIS、介质更新、RR 和线性累积。`mod_scene.slang` 翻译网格、atlas、tint 与环境，源包默认 roughness=0.9、IOR=1.5 的 opaque；native quad 另可选择 roughness=0.25 的 opaque 预设，相机初始介质为空气。复用引擎 Earth 大气：太阳盘使用 NEE/MIS 和 alpha 阴影，环境来自 SkyView，主射线命中后逐样本合成 Aerial L+T。太阳盘在大气外的亮度标定沿用 game 默认 20_000；海平面对应 0.5 km，每世界单位对应 1 m，Aerial 最远 64 km。暂无局部光源和发光。默认每帧 1 spp、12 顶点。原始 PT 在视图、太阳、海平面、积分参数、mesh 或 atlas 变化时清空累积；RR 使用独立帧历史，普通相机与太阳运动保持历史，尺寸/投影、瞬移、场景、开关和求值失败重置，曝光只作用于呈现。

主查询 any-hit 与阴影 RayQuery 候选均以 alpha≥0.5 接受交点。atlas 使用 sRGB 格式，tint 解码后在线性空间相乘；几何点和误差界复用引擎 `geometry::triangle_point`。公共 HDR 左上原点，独立呈现阶段使用 Prime DRT、sRGB 编码和宿主 Y 翻转。

atlas 重载扫描本适配器实际使用的 LOD0 与 tint 范围，推导通用 `FlowKey`，在发布前通过 Slang 内容缓存编译 callable 并初始化 pipeline/SBT。atlas、能力证明与程序全部成功后一起替换，失败保留上一代。缓存位于 `target/shaders/`；首次加载和资源重载需要可用的 Slang 及本 checkout 的受控 shader 源码。绘制、尺寸和 TLAS 变化只重绑资源，复用现有程序；当前 base/tint 接口仍不提供额外 PBR 纹理通道。

## 构建与运行

需要 Java 25、Rust、Vulkan 1.4、`VK_KHR_device_address_commands` / `deviceAddressCommands`、Slang（`SLANGC` / `VULKAN_SDK` / PATH）和支持 Vulkan RT Pipeline、RayQuery、`VK_KHR_ray_tracing_position_fetch` / `rayTracingPositionFetch`、`shaderStorageImageExtendedFormats` 的显卡。RR 另需 Windows、支持 DLSS Ray Reconstruction 的 NVIDIA GPU/驱动及 Streamline 运行时；Gradle SDK 校验任务使用 PowerShell 7 (`pwsh`)。Gradle wrapper 已包含。

```powershell
Set-Location C:\repositories\simple_engine\experiments\prime_pt
.\scripts\setup-streamline.ps1
cargo test --lib
.\gradlew.bat wireFixture
cargo test --lib -- --ignored --skip hardware_pt_rr_full_chain --test-threads=1
cargo test --lib gpu::smoke::hardware_pt_rr_full_chain -- --ignored --nocapture --test-threads=1
cargo doc --no-deps
.\gradlew.bat build
.\gradlew.bat runClient
```

`setup-streamline.ps1` 调用引擎安装器并设置当前进程 `STREAMLINE_SDK`，默认缓存为引擎 `target/streamline-sdk`，校验签名/hash；已有副本可用 `-VerifyOnly` 核验。native 构建自动将 `sl.interposer.dll`、`sl.common.dll`、`sl.dlss_d.dll`、`nvngx_dlssd.dll` 部署到 `prime_pt.dll` 同目录。SDK bridge 由引擎 `vulkan/dlss_rr` 提供，mod 只提供应用身份和宿主适配。

`runClient` 先构建 release DLL，再使用 `--graphicsBackend VULKAN` 与 `--enable-native-access=ALL-UNNAMED` 启动。JAR 位于 `build/libs/`，DLL 位于 `target/release/prime_pt.dll`；也可通过 `-PnativeLibrary=绝对路径` 指定已有 DLL，此时同目录须有上述四个 SDK DLL。`runClient -Prr=false` 完全跳过早期 SDK 接入，仅运行原始 PT；该启动开关生效后，重新启用 RR 需要重启。

SER 默认自动检测并启用：宿主协商 `VK_EXT_ray_tracing_invocation_reorder` / `rayTracingInvocationReorder`，创建回调确认逻辑设备已启用后才使用 NV `render-ser.spv`。缺少能力时记录原因并使用普通 RT pipeline。视频设置或 `/pt ser on|off` 即时切换，偏好保存到 `ser=true|false`，不需要重启；开关不清空 PT/RR 历史。`runClient -Pser=false`（普通启动器 `-Dprimept.ser=false`）仅为缺少 `ser` 键的配置提供初始默认值，已有持久化值优先。恢复默认重新设为自动开启。设备能力始终保留，不受当前偏好影响。

启动只创建当前选择的程序。改变 SER 偏好时同步创建另一组 pipeline/SBT，复用现有 callable SPIR-V、atlas、mesh/AS、视图图像和缓冲；不重新扫描纹理或调用 Slang。创建成功后替换，失败保留旧程序；旧命令由 Flight 保活，下一次已完成的录制边界退役。稳态只保留当前程序，切换期间短暂同时保留新旧管线/SBT 和两组描述符，不复制场景或图像。当前只有一个材质 callable，K2 在 `CallShader` 前使用无 hint 的 `ReorderThread`，不能预期所有场景都获益。运行 `cargo test --lib hardware_pt_ser_ -- --ignored --nocapture --test-threads=1` 验证真实运行时开关的 HDR/呈现（既有 PT `1e-6` scaled error 约定）、累积历史和不支持时的普通路径，检查 Vulkan validation；params/counts/prefix 保持逐位相同。本机普通程序重建也可产生至多 3 ULP 差异，不承诺 HDR 逐位相同。

默认开启 Performance DLSS RR：输入宽高为输出的 1/2，放大至宿主分辨率。可选 DLAA、Quality、Balanced、Performance、Ultra Performance，具体输入尺寸由 SDK 查询并校验；关闭 RR 使用原分辨率 PT。`/pt rr` 查询请求开关与 SDK/设备就绪状态，`/pt rr on`、`/pt rr off` 即时切换；初始化/配置失败缓存原因并回退原始 PT，关闭再开启、切换档位或窗口尺寸变化可重试。每帧求值安全失败使用通用 5×5 空间滤波并重置历史，原因见 native 日志。SDK 在创建任何 Vulkan 对象之前安装，LWJGL 与 SDL3 的私有 loader 统一指向 interposer。缺少 DLL、设备能力或早期初始化失败会明确记录原因，继续原始 PT。加载 SDK 的机器即使暂时关闭 RR，也保留早期 loader 和协商的设备能力。没有 Frame Generation 或 Reflex。

可选 `.\gradlew.bat streamlineFixture` 在独立进程中验证早期 bootstrap，并确认 SDL 与 LWJGL 的 `vkGetInstanceProcAddr` 地址一致；它不创建窗口或设备，需先构建 native DLL 和部署 SDK，不属于缺少 SDK 时也能运行的常规 `check`。

视频设置在顶部提供恢复默认值按钮，分为“PT 渲染”（SER、RR、质量、SPP、顶点预算）和“PT 显示”（曝光），之后保留原版分栏。界面与聊天命令共用设置模型，立即保存至游戏目录的 `config/prime_pt.properties`。原有曝光和 RR 开关保留；缺少新配置时读取旧 `primary_rt.properties`，后续保存到新路径，新质量档默认为 Performance。命令在本地处理，无需服务端权限或 Fabric API，省略值表示查询；

| 命令 | 可调值与默认值 |
| --- | --- |
| `/pt ser [on\|off]` | 默认 on，设备不支持时使用普通程序 |
| `/pt rr [on\|off]` | 默认 on |
| `/pt quality [档位]` | `dlaa`、`quality`、`balanced`、`performance`（默认）、`ultra_performance` |
| `/pt ev [EV]` | [-16, 16]，允许小数，默认 0；`exposure` 为别名 |
| `/pt spp [样本数]` | [1, 64]，默认 1；RR 下平均同一 jitter 的多个独立样本 |
| `/pt bounces [路径顶点数]` | [1, 64]，默认 12 |
| `/pt reset` | 恢复上述默认值 |

`/pt` 汇总当前设置。聊天框使用原版 Brigadier 提示、参数说明与 Tab 补全；本地命令树随服务器 dispatcher 更新重新接入，执行仍只在客户端处理。无效输入或保存失败保持原值；曝光 +1 EV 使亮度加倍，只影响呈现，不清空 PT 历史或重建大气。

太阳方向薄捕获自 26.3 原版相机 `EnvironmentAttributes.SUN_ANGLE`，使用相同的 partial tick 插值；海平面来自当前世界。只在 `Skybox.OVERWORLD` 启用太阳/大气，Nether、End 和无天空维度暂为黑色环境。Java 的太阳变换、设置命令、旧配置迁移与持久化 fixture 纳入 Gradle `check`；它们不启动 Minecraft。

游戏手工验收：进入世界后等待附近地形出现；检查草叶 tint、树叶透明孔、负坐标附近地形、放置/破坏方块、chunk 卸载、跨 64 格边界、资源重载、窗口缩放和退出。编译与无窗口 GPU 验证不能代替游戏验收。

无窗口测试覆盖主查询 cutout、RayQuery/TraceRay 的 alpha 边界一致性、纹理/tint 线性约定、宿主上下方向、跨帧样本合批、相机/太阳历史失效、太阳遮挡、天空方向、Aerial 距离和无天空维度。曝光测试覆盖正负 EV、历史连续和同一绝对样本序列的 HDR 逐字节一致，并通过借用宿主 command 的 `Recorded::encode` 路径执行。原始 PT 测试设备强制启用 RT/RayQuery、position fetch 与 extended storage formats，不依赖 OMM；另验证非共面梯形 quad 与两个退化 quad 的几何/UV/覆盖率等价。RR 测试使用独立进程和生产 interposer 路径，实际执行重建并读回数据。

K1 的通用接口 `pt::IPrefixGuides` 在 alpha 接受后的首个实际表面生成法线/线性粗糙度与 OpenPBR 方向 diffuse/specular albedo，并提供正 view-Z、去 jitter 的像素运动和天空投影；默认关闭不增加 game 的资源或输运成本。mod 将它们打包为 R32F depth、RG16F motion 和 RGBA16F 材质数据，使用全帧 Halton jitter 与本帧 SPP 的线性 HDR 均值输入 RR，重建后才执行曝光/DRT。specular motion 使用额外理想反射射线的局部平面代理，粗糙表面并非光学 PSR；当前只支持静态方块几何，没有透明覆盖 alpha 重建。

## 初版边界

- 维护相机周围 3³ regions（192³ 方块）的窗口；跨 region 清空并分帧重捕，加载期间会有空缺。block、chunk 和 tint 变化会使对应 section 失效。
- 支持公共方块模型源与位置偏移；缺少流体、实体、方块实体和粒子。动画纹理固定第一帧，资源重载重新捕获。未接入第三方自定义模型回调。
- 原版世界提取与绘制已截断，保留相机、玩家状态、手部和 UI。宿主适配固定为 26.3；没有其它渲染 Mod 的兼容保证。
- 每次几何变化重新合并场景并重建整个 BLAS/TLAS；每帧显式等待宿主提交完成，再复用单份 GPU 资源。没有异步加载或性能承诺。
- Java 以 f64 传递绝对相机位置，CPU mesh 保留 section 局部坐标；合批先减去相机所在 64m 网格 anchor 再转 f32。anchor 变化会重建 AS 并重置图像历史。尚未实现 Minecraft 的透射、发光或专用材质规则。
- CPU 保留 section mesh；更新时暂存本批 mesh、全场景合并副本和 AS 三角形输入。单场景最多 16M 顶点，单源包/atlas 最多 256 MiB，另受设备 storage binding 上限约束。
- 视图保留一份 scratch，当前 opaque/air/prefix=false 的五 plane 交接为60 B/输入像素；PT 工作缓冲合计112 B/输入像素，RR 开启时另增 48 B/输入像素和 8 B/输出像素（关闭仅保留 1×1 占位），另有 SDK 自有缓存，独立于每批样本数。大气拥有固定 Earth 场与 SkyView/Aerial 缓存。宿主输出 view 只借到本帧完成，呈现 program 复用，每帧只重建绑定。

quad 取舍、逐顶点属性边界与48 B常驻属性 ABI见 `native/src/scene.rs::Quad` rustdoc；位置强制从 AS 获取，未提供无 position fetch 的回退。60 B landing 是当前调度/材质子集专门化，扩展介质、发光或 delta 前缀必须发布新的pipeline/资源 ABI。CPU section quad为96 B，dirty构建时另存48 B属性打包和72 B三角形输入；GPU常驻量不包含AS内保留的位置成本。

跨语言布局和借用契约见 `native/src/lib.rs` 的导出接口 rustdoc，GPU 所有权见 `native/src/gpu.rs`，Java 捕获见 `java/src/main/java/dev/primept/Capture.java`。
