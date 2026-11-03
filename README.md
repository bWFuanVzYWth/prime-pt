# Prime PT

Minecraft 26.3 / Fabric 的 Prime PT 重写，参考旧 Prime PT 的代码与契约。此目录是独立 Git 仓库和 Cargo workspace，依赖上两级的 simple_engine；引擎不依赖本目录。`target/`、`run/` 和 SDK 缓存不纳入版本管理。

引擎 `chunk_manager::State` 独占 CPU region 资源，只调度 LOD0；水平范围由宿主有效视距（16m chunk 列）决定，Y 覆盖世界完整高度，向外对齐世界原点的 64m region。每 region 合并4×4×4个 section，非空 region 只建立一个 BLAS。Java 每帧按 native ticket 薄捕获至多64个 section，并在每个 section 完成后检查3ms预算，未回复请求下帧保留原ID继续：转发原版模型的 quad、atlas UV、面可见性和群系 tint。Rust 在批次层调度 CPU 工作池，缓存 region 合并网格、section 范围及 revision；与 game 共用 `scene_cache` 资源事务，仅为几何变化的 region 展开三角形并重建 BLAS，以平移实例组成 TLAS。quad 属性仍全量打包上传，使用实例的属性基址直接寻址；引擎 `vulkan::native` 拥有 GPU 资源和 BLAS/TLAS，`pt::gpu::Renderer` 录制 K1→K2→K3。使用 RT Pipeline、叶子材质 callable 与 SBT；Slang 直接生成 SPIR-V。

与 game 共用 `shaders/pt/{integrator,path_sampler,post}.slang`，包含 OpenPBR、Beer、NEE/MIS、介质更新、RR 和线性累积。`mod_scene.slang` 翻译网格、atlas、tint 与环境，源包默认 roughness=0.9、IOR=1.5 的 opaque；native quad 另可选择 roughness=0.25 的 opaque 预设，相机初始介质为空气。复用引擎 Earth 大气：太阳盘使用 NEE/MIS 和 alpha 阴影，环境来自 SkyView，主射线命中后逐样本合成 Aerial L+T。太阳盘在大气外的亮度标定沿用 game 默认 20_000；海平面对应 0.5 km，每世界单位对应 1 m，Aerial 最远 64 km。暂无局部光源和发光。默认每帧 1 spp、12 顶点。原始 PT 在视图、太阳、海平面、积分参数、mesh 或 atlas 变化时清空累积；RR 每帧重新生成当前帧 HDR，只平均本帧 SPP；SDK 独立维护时间历史。相机、FOV、太阳、积分预算、局部场景与 anchor 变化保留 RR 历史；世界或材质资源域换代显式重置，实际视图/SDK 存储替换及求值失败分别冷启动或恢复，曝光只作用于呈现。

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

默认自动曝光复用引擎的 GPU 对数直方图测光：两端各裁去0.5%像素，暗化／亮化分别以0.5／2秒消除90%的目标EV差值。原始PT测量未曝光的线性HDR；RR成功时测量重建后的线性HDR，求值回退时测量本帧原始HDR。手动EV作为最终曝光补偿，与自动倍率相乘后才进入Prime DRT；两者均不改变输运、线性累积或大气。曝光历史独立于PT/RR历史，相机、场景和尺寸变化继续适应，首次有效帧直接建立测光目标。测光录制并入宿主command，不增加CPU回读或逐帧GPU等待。

太阳方向薄捕获自 26.3 原版相机 `EnvironmentAttributes.SUN_ANGLE`，使用相同的 partial tick 插值；海平面来自当前世界。只在 `Skybox.OVERWORLD` 启用太阳/大气，Nether、End 和无天空维度暂为黑色环境。Java 的太阳变换、设置命令、旧配置迁移与持久化 fixture 纳入 Gradle `check`；它们不启动 Minecraft。

F3+2 使用 Prime 分项卡顿图替换左侧 FPS 图，保留右侧宿主 TPS 图。完整帧间隔、1秒窗口、P90/P99、按像素保留最长整帧和持久卡顿详情复用引擎 `frame_loop::timing`，与 game 同源。14个分项分别记录宿主等待、源/材质、规划、宿主捕获、网格编译/合并、三角编码、属性打包、BLAS构建/等待、属性分配/写入、TLAS构建/等待、场景提交/回收、native录制/SDK、宿主提交及未归因；内部场景耗时从录制总时长扣除，互不重复。选中柱或最近卡顿单独标出最大耗时项、毫秒数及整帧占比；另显示最近场景更新序号、dirty region/实例数量、三角输入和属性字节数，后者不冒充所选历史帧的规模。全部为主线程墙钟，不冒充GPU时间。属性段包含新建VkBuffer、分配内存及map/copy/unmap，没有队列提交；非空几何更新的BLAS批与TLAS各有一次同步提交/等待，计时包含主机命令资源开销。图隐藏时只维护复用数值缓冲，不格式化文本；不会增加GPU回读或等待。

游戏手工验收：进入世界后等待附近地形出现；检查草叶 tint、树叶透明孔、负坐标附近地形、放置/破坏方块、chunk 卸载、跨 64 格边界、资源重载、窗口缩放和退出。编译与无窗口 GPU 验证不能代替游戏验收。

无窗口测试覆盖主查询 cutout、RayQuery/TraceRay 的 alpha 边界一致性、纹理/tint 线性约定、宿主上下方向、跨帧样本合批、相机/太阳历史失效、太阳遮挡、天空方向、Aerial 距离和无天空维度。曝光测试覆盖正负 EV、历史连续和同一绝对样本序列的 HDR 逐字节一致，并通过借用宿主 command 的 `Recorded::encode` 路径执行。`hardware_pt_auto_exposure_` ignored测试覆盖原始PT的测光呈现、HDR不变、resize适应历史与手动EV补偿；RR完整链路先显式关闭自动曝光以保持原图像对照，末尾再启用自动曝光，验证真实RR求值后的测光状态与输出尺寸。原始 PT 测试设备强制启用 RT/RayQuery、position fetch 与 extended storage formats，不依赖 OMM；另验证非共面梯形 quad 与两个退化 quad 的几何/UV/覆盖率等价。RR 测试使用独立进程和生产 interposer 路径，实际执行重建并读回数据。

K1 的通用接口 `pt::IPrefixGuides` 在 alpha 接受后的首个实际表面生成法线/线性粗糙度与 OpenPBR 方向 diffuse/specular albedo，并提供正 view-Z、去 jitter 的像素运动和天空投影；默认关闭不增加 game 的资源或输运成本。mod 将它们打包为 R32F depth、RG16F motion 和 RGBA16F 材质数据，使用全帧 Halton jitter 与本帧 SPP 的线性 HDR 均值输入 RR，重建后才执行曝光/DRT。specular motion 使用额外理想反射射线的局部平面代理，粗糙表面并非光学 PSR；当前只支持静态方块几何，没有透明覆盖 alpha 重建。

## 初版边界

- 视距按相机所在水平列两侧的闭区间解释，region 向外对齐；Y 使用世界半开 section 高度域，不按相机高度裁掉地形。固定8192槽，目标 region 数量的两倍加64过渡余量必须能容纳，超额明确报错。移窗保留交集、有效票据和最近已解析窗口，过期中间前沿允许淘汰。未加载 section 显式回报不可用，撤下整个未闭合 region 并缓存不可用状态；加载通知触发完整64 section 重捕，真实空与不可用分开。
- 支持公共方块模型源与位置偏移；缺少流体、实体、方块实体和粒子。动画纹理固定第一帧，资源重载重新捕获。未接入第三方自定义模型回调。
- 原版世界提取与绘制已截断，保留相机、玩家状态、手部和 UI。宿主适配固定为 26.3；没有其它渲染 Mod 的兼容保证。
- section 级64bit脏标记只重捕变化源，再将该 region 的保留范围与新范围合并。几何变化按 revision 复用未变 region 的 BLAS，并重建 TLAS；quad 数量或按顺序的 P48 位置位值改变才推进几何 revision，相同源包及 tint/UV 更新保留 BLAS。真正几何编辑会重建所属整个 region BLAS，不能把“只重捕一个 section”理解为“只重建一个 section”。属性按当前活动槽全量重新打包上传，尚无分段属性传输。每帧显式等待宿主提交完成，再复用单份 GPU 资源。没有异步加载或性能承诺。
- Java 以 f64 传递绝对相机位置，CPU mesh 与 BLAS 保留 region 局部坐标；TLAS 实例平移先减去相机所在 64m 网格 anchor 再转 f32。anchor 变化保留局部 BLAS、更新 TLAS，上一帧世界相机重投影到当前 anchor，保留 RR 历史。尚未实现 Minecraft 的透射、发光或专用材质规则。
- CPU 只保留一份 region 合并 mesh 与64个 section 范围；最多8个在途 region job 拥有阶段性脏 section 输出。变化 region 的三角输入汇入一个连续 dirty 集，由 native 批入口共用上传与 scratch、一次提交并等待。批返回后释放 CPU 三角与描述符，再打包完整属性槽，不再生成全场景位置合并副本。属性槽准备时保留旧范围，新场景成功后才回收；工作集须计入完整 dirty 集、旧/新属性缓冲、变化 BLAS、TLAS 与构建暂存。单场景最多 16M 顶点，单源包/atlas 最多 256 MiB，另受设备 storage binding 上限约束。
- 视图保留一份 scratch，当前 opaque/air/prefix=false 的五 plane 交接为60 B/输入像素；PT 工作缓冲合计112 B/输入像素，RR 开启时另增 48 B/输入像素和 8 B/输出像素（关闭仅保留 1×1 占位），另有 SDK 自有缓存，独立于每批样本数。大气拥有固定 Earth 场与 SkyView/Aerial 缓存。宿主输出 view 只借到本帧完成，呈现 program 复用，每帧只重建绑定。

quad 取舍、逐顶点属性边界与48 B常驻属性 ABI见 `native/src/scene.rs::Quad` rustdoc；位置强制从 AS 获取，未提供无 position fetch 的回退。60 B landing 是当前调度/材质子集专门化，扩展介质、发光或 delta 前缀必须发布新的pipeline/资源 ABI。CPU section quad为96 B，dirty构建时另存48 B属性打包和72 B三角形输入；GPU常驻量不包含AS内保留的位置成本。

跨语言布局和借用契约见 `native/src/lib.rs` 的导出接口 rustdoc，GPU 所有权见 `native/src/gpu.rs`，Java 捕获见 `java/src/main/java/dev/primept/Capture.java`。
