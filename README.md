# Prime PT

Minecraft **26.3 / Fabric** 的 Prime PT 重写，复用 simple_engine 的 Slang、OpenPBR、PT、Vulkan 与重建接口。本目录是独立 Git 仓库和 Cargo workspace，依赖上两级引擎；引擎不依赖实验。开发边界见 [AGENTS.md](AGENTS.md)，实现契约以链接的 Rust API 为准。`target/`、`build/`、`run/`、SDK 与测量缓存不入库。

## 结构与数据流

| workspace package | 职责与入口 |
| --- | --- |
| `prime_pt` | [native](native/src/lib.rs)：宿主 FFI、资源/世界事务、命令与完成边界、诊断快照 |
| `prime_minecraft` | [minecraft](crates/minecraft/src/lib.rs)：26.3 压缩源、模型/材质准备、剔面、流体、slab 编译和原版 tint |
| `prime_world` | [world](crates/world/src/lib.rs)：引擎 LOD0 manager、票据、源缓存、显式 CPU 调度、预算和发布 |
| `prime_scene` | [scene](crates/scene/src/lib.rs)：宿主无关的 quad、材质身份、相机和借用渲染视图 |
| `prime_renderer` | [renderer](crates/renderer/src/lib.rs)：Vulkan 场景缓存、资源/程序、PT/RR、曝光与呈现 |

Java 只绑定 Minecraft 类型与源字段，转发事件和宿主句柄。[SourceResources](java/src/main/java/dev/primept/SourceResources.java) 在首次加载/资源重载捕获完整注册状态、模型 DAG、剔除形状、sprite 和 tint recipe；[Capture](java/src/main/java/dev/primept/Capture.java) 运行期按 native ticket 复制真实 section 的 palette/bit storage，使用 **PRT3**，不逐 block 选择模型、剔面或展开 quad。每批至多 64 section，每完成一个 section 检查 3 ms 捕获预算，未回复 ticket 保留到后续帧。

宿主变化由单一真实来源回调捕获，按世界、列、section 范围合并后以 PDI1 批量转发；几何、源替换、biome 和颜色失效分开处理，失败保留未确认批次。资源重载走独立世代事务。入口见 [SourceChanges](java/src/main/java/dev/primept/SourceChanges.java) 与 [变化包边界](native/src/changes.rs)。

Rust 校验同世代资源与完整邻域后，以四层 slab 为批量内核执行模型选择、位置偏移、剔面和流体几何；工作池只由 world 调度。草、叶、干叶、水、高草、红石、藤茎等原版 tint 的 zoom、颜色表、noise 和 box blend 在 [tint](crates/minecraft/src/tint.rs) 计算，世界 seed、biome palette 与已捕获 quart 按世界缓存；Java 只回报缺失的原始 quart 身份。自定义 tint 函数保留显式批量宿主回退，结果齐备后才能发布 mesh。

## 资源准备与增量更新

首次加载和资源重载是完整资源准备边界。一个世代只有一个全局 Catalog，section、region 和工作线程引用稳定身份；材质范围推导、静态发光纹理 alias、Slang callable、pipeline/SBT 及设备支持的普通/SER 变体在发布前准备。运行期不按方块、区块或首次命中懒构建 alias、shader 或 SBT；缺身份/错世代显式报错。新世代全部成功后统一发布，失败保留旧世代，旧 GPU 资源按真实完成证明回收。入口见 [Catalog](crates/minecraft/src/resources.rs)、[材质准备](crates/minecraft/src/materials.rs) 和 [Gpu](crates/renderer/src/gpu.rs)。

world 只使用引擎 `chunk_manager` 的 LOD0。水平目标来自宿主有效视距，覆盖完整世界高度，向 64 m region 对齐；每 region 管理 4³ 个 16 m section，一个非空 region 一个 BLAS。相机以 f64 保存绝对位置，CPU 几何/BLAS 保留 region 局部坐标，TLAS 平移减去 64 m anchor 后转 f32。移窗保留需求交集与有效 ticket，完全退出需求域的 region 独立卸载；边缘未加载源不会永久阻塞域外退休。未加载、真实空和预算排除有不同状态。

CPU 以四层 slab 持有独立输出，一个 section 的四个内核共享只读邻域，直接生成 region 局部坐标。准备视图借用旧片段和变化输出，GPU 成功后移动发布，避免 section 拼接、坐标二次平移和完整 region 复制。持久 tint 绑定引用资源世代内稳定配方身份，纯颜色刷新借用旧几何并提交属性范围。几何 revision 变化才重建所属 region 的 BLAS；tint/UV/材质属性变化保留 BLAS。dirty 三角输入共享一次批构建与 scratch，非空 BLAS 批有一次同步提交/完成，TLAS 按实例集合或 anchor 变化重建。每帧修改/复用前仍等待此前宿主命令完成，当前不是异步加载流水线。

同 region 刷新遵循引擎 [State](../../crates/chunk_manager/src/lib.rs) 的无缝发布契约：源、halo、tint 或候选尚未齐备，以及预算不足、GPU 准备失败时，继续显示旧 mesh；新属性、BLAS/TLAS 和最终绘制集合准备成功后一次切换，旧资源按实际完成边界退役。未知或不可用源只能记录负缓存，不能冒充真实空网格撤下旧几何。完整编译结果确认为空时，才允许原子清除该覆盖。LOD 父子替换使用相同边界，Prime 当前仍只使用 LOD0。

GPU 的 48 B/quad 属性缓冲持久保留容量：容量内只打包和映射写入变化范围，无内容变化或仅 anchor 变化不重写属性；扩容才新建缓冲并重写全部活动属性。纯属性更新保留 TLAS。活动发光 quad 使用稳定数据槽、紧凑活动选择表和反查索引；颜色刷新不扫描灯，灯集合/槽位变化只准备变化行，增长时才完整重建容量表；运行期均匀选择灯并使用实际离散 PMF，不重建全局 alias。纹理内 alias 和静态面积复用资源准备产物，过程流体的两三角面积可在 shader 计算。

默认 CPU quad 逻辑预算为 resident **1 GiB**、staging **256 MiB**、单 region **256 MiB**，不包含源缓存、Vec 余量、线程、驱动或 GPU 内存。超额以 [Pressure](crates/world/src/budget.rs) 区分最终容量、过渡等待和单 region 容量；暂停或排除新候选时保留已有覆盖，不能靠提前撤下同一覆盖解锁容量。下调预算时已发布几何可能高于新限制，持续报告压力并阻止新增，直到预算提高或实际域外资源退休；不宣称此时已满足新上限。manager 从实际占用按需扩容；显式 [WorkBudget](crates/world/src/budget.rs) 在一次 plan 开始的推进轮中约束内核、规划访问、发布及维护，后续源包和颜色回填共享剩余额度。缺源或容量受阻的任务不能耗尽建网额度并阻挡可执行任务。调度限额与恢复条件见 [Scene](crates/world/src/chunks.rs)。

## 渲染能力与边界

与 game 共用引擎 `shaders/pt` 的积分器、样本身份、Beer、OpenPBR、太阳/局部发光 NEE/MIS、重建和呈现算法。材质表提供 MERS、作者法线/分布粗糙度/IOR、双面规则、玻璃/薄面、水面和水下初始介质；发光受真实 UV、tint、覆盖及发射侧约束。cutout 使用 alpha≥0.5，透射/流体不套用该二值规则；主查询与阴影 RayQuery 共用接受规则。着色法线不替代几何偏移和介质边界法线。实际源翻译见 [materials](crates/minecraft/src/materials.rs) 与 [场景 shader](crates/renderer/shaders/mod_scene.slang)。

原版 PBR 使用明确 sprite 身份对应的冻结数据，并保留 [来源锁](crates/minecraft/assets/vanilla_pbr/source.lock.json) 与 [Mojang 许可声明](crates/minecraft/assets/vanilla_pbr/SOURCE-LICENSE.md)；这些资产不因代码仓库的 [LICENSE](LICENSE) 改变许可。外部 `_n`/`_s` 贴图目前未消费，资源世代准备检测到时汇总一次 WARNING；已有图片协议字段不代表支持外部 LabPBR。资源动画字段/包络准备不等于播放：**当前材质 atlas 固定准备时首帧，尚未逐 tick 同步颜色/PBR 动画**。

复用 Earth 大气、太阳盘与 Aerial L+T；太阳角薄捕获自原版环境属性，海平面来自世界，仅 Overworld skybox 启用，Nether/End 暂为黑色环境。HDR 为线性 BT.709，albedo sRGB 解码后乘线性 tint，最后自动曝光/手动 EV、Prime DRT 和宿主方向呈现。

当前仍有以下范围限制：

- 运行普通 PT，未接 ReSTIR；**Prime 当前未启用 OMM**，主查询/阴影依赖 any-hit/RayQuery 覆盖。引擎有 OMM 能力不代表本应用已经接入。
- 支持已识别的公共模型 DAG 和原版位置/流体规则；未知自定义模型源显式拒绝。自定义 tint 的批量回退不代表自定义模型兼容。
- 实体、方块实体和粒子尚未接入；宿主相机、玩家状态、手部和 UI 继续使用原版路径，没有其它渲染 Mod 的兼容保证。
- RR guide 复用引擎首命中、delta 链及反射/折射代理，但场景姿态仍按静态方块提供。粗反射运动、折射与预算终点有代理边界，不是动态物体/透明多层的完整光学运动重建。

## 构建与运行

需要 Java **25**、仓库固定的 Rust nightly、Slang、PowerShell 7，以及实际支持 Vulkan **1.4** 的 loader/设备。强制能力包括 RT Pipeline、RayQuery、buffer device address、`VK_KHR_ray_tracing_position_fetch`、`VK_KHR_device_address_commands`、synchronization2 和 extended storage image formats；没有 position-fetch 顶点副本回退。逻辑设备实际启用的能力由 [Bootstrap](java/src/main/java/dev/primept/Bootstrap.java) 确认，材质 `FlowKey`/程序特化不能用物理支持代替逻辑设备能力。首次资源准备仍需可用 Slang 和本 checkout 的受控 shader 源码。

RR 另需 Windows、支持 DLSS Ray Reconstruction 的 NVIDIA GPU/驱动及锁定 Streamline SDK。SDK 版本、来源/hash、部署与验证边界见引擎 [Streamline 手册](../../crates/vulkan/streamline/README.md)。

从本目录执行：

```powershell
.\scripts\setup-streamline.ps1
cargo test --workspace --lib
cargo doc --workspace --no-deps
.\gradlew.bat check
.\gradlew.bat compactSourceFixture modelRandomFixture tintFixture
cargo test -p prime_minecraft --lib -- --include-ignored
.\gradlew.bat build
.\gradlew.bat runClient
```

安装器默认使用引擎 `target/streamline-sdk` 并设置当前进程 `STREAMLINE_SDK`；已有 SDK 可加 `-VerifyOnly`。Slang 从配置的 `SLANGC`/`VULKAN_SDK` 或 PATH 查找，构建不下载 SDK。`runClient` 构建 release DLL 后以 Vulkan backend 启动；JAR 在 `build/libs/`，DLL 在 `target/release/prime_pt.dll`。指定 `-PnativeLibrary=绝对路径` 可复用已有 DLL，其同目录必须保留安装器部署的四个 SDK DLL 与许可。`runClient -Prr=false` 跳过早期 SDK 接入，只运行原始 PT，之后启用 RR 需要重启。游戏验收由用户执行。

SER 在实际逻辑设备支持时默认开启，普通和 SER pipeline/SBT 在资源世代准备时一并建立，运行期只切换已准备的程序；不因偏好切换重新扫描资源、调用 Slang 或创建 SBT。缺能力时使用普通程序并记录原因。`-Pser=false` 只为尚无持久化设置的配置提供默认值。

视频设置与本地 `/pt` 命令共用 `config/prime_pt.properties`，无须服务端权限；省略值查询，`/pt reset` 恢复默认：

| 命令 | 默认与范围 |
| --- | --- |
| `/pt ser on\|off`、`/pt rr on\|off` | 默认 on；实际能力不足会明确回退 |
| `/pt quality 档位` | `dlaa` / `quality` / `balanced` / `performance`（默认）/ `ultra_performance` |
| `/pt spp 数量`、`/pt bounces 顶点数` | 默认 1 / 12，均为 1..64 |
| `/pt ev EV` | 默认 0，-16..16；自动曝光后的补偿，`exposure` 为别名 |

RR 默认 Performance，输入每轴约为输出一半，实际尺寸由 SDK 查询。**RR 不跨帧做线性样本累积**：当前帧 SPP 求均值后输入 SDK，时间历史由 SDK 独立维护。相机、FOV、太阳、积分参数、局部场景和 anchor 变化保留 RR 历史；世界/材质资源域换代显式重置，实际视图或 SDK 存储替换、求值失败分别冷启动/恢复。原始 PT 在其输入变化时清空线性累积。曝光单独适应，只影响呈现。

SDK 在任何 Vulkan 对象前安装 interposer；初始化失败记录原因并回退原始 PT，每帧安全求值失败使用空间滤波并重置历史。没有 Frame Generation 或 Reflex。RR 测试必须独立进程运行，不能将关闭同步验证当作修复 SDK validation 错误；具体已知现象见 Streamline 手册。

## 诊断、基准与验收

F3+2 用 Prime 分项图替换左侧 FPS 图，保留宿主 TPS 图。统计与按像素保留最长整帧的采样复用引擎 `frame_loop::timing`；14 个互斥分项覆盖等待、tint/源维护、规划/捕获、编译/三角、属性与光源打包/写入、BLAS、TLAS、场景准备/提交、录制/SDK、宿主提交和未归因。`plan` 内同步推进的源编译计入网格项，视图准备归场景准备/提交；网格项也包含解码、预算核算和合并，并非纯建网内核。最大项显示毫秒及占比，预算压力显示排除数、延后数及所需/限制容量；场景更新序号/规模明确属于最近更新，不冒充所选历史帧。全部为**主线程墙钟**，不是 GPU timestamp。工作状态显示建网 slab 已用/额度、ready 队列、本轮遇缺源/容量、发布、复用和取消数；Rust 通知 raw→unique 不包含已在 Java 合并的原始宿主回调数量。图隐藏时只记录，不生成或传输 PFT2 快照；保峰直接比较整帧 u64 纳秒，不构造未消费的 float 图表。读取诊断不增加 GPU 回读/等待。入口见 [diagnostics](native/src/diagnostics.rs) 和 [FrameChart](java/src/main/java/dev/primept/FrameChart.java)。

CPU 同源基准无需游戏/GPU：

```powershell
New-Item -ItemType Directory -Force ../../temp | Out-Null
$env:PRIME_WORLD_BENCH_OUTPUT = Join-Path (Resolve-Path ../..).Path "temp/prime-source-$((Get-Date).ToString('yyyyMMdd-HHmmss'))"
cargo bench -p prime_world --features test-fixtures --bench source_pipeline
```

[source_pipeline](crates/world/benches/source_pipeline.rs) 保存实际 PRM1/PRT3、PTR1/PBW1 输入及 hash、原始 `samples.jsonl` 和边界说明。输入覆盖空源、密集/稀疏模型、流体、tint、27 邻域与 1/8 worker 的生产 world 调度；资源字典和私有池建立在计时外，私有池广播等待全部 worker 就绪并跨样本复用，clear/资源绑定不计入样本；world 段包含规划、fixture 源包写入、decode、slab fork/join、候选发布与 CPU 提交。新增视距 4/32 的初载、稀疏编辑和 256 轮稳态消费者，输出完整属性/位置摘要在计时外生成。world 的固定 ARGB fixture 回填不含真实宿主 tint 回调成本。独立 `vanilla_tint` 段测真正 plan/finish 的冷/热缓存、radius 0/7、失效和自定义回退响应分配；两类测量不能相加冒充完整帧耗时。Java 捕获、GPU 与全局资源准备另计。

完整 world 用例每轮新建私有池；构造调用虽在计时外，worker 就绪和前一池退出未显式等待，仍可能与首波计时重叠。该用例包含冷启动影响，尤其多线程空源结果不能代替实机常驻线程池的暖测。原始漂移与输入身份须一起保留。

GPU 场景基准需要独占设备，输出必须使用新目录：

```powershell
$env:PRIME_PT_SCENE_BENCH_OUTPUT = "run-$((Get-Date).ToString('yyyyMMdd-HHmmss'))"
cargo test --release -p prime_renderer --lib hardware_pt_scene_baseline -- --ignored --nocapture --test-threads=1
```

[scene_bench](crates/renderer/src/scene_bench.rs) 以真实 manager ticket 驱动 1728 section/27 region，固定 64 个非空 section、98,304 quad、1024×576、1 spp、12 顶点、RR 关闭。它仍使用兼容 PRT2 合成几何输入，测 manager/GPU 更新，不能替代新 Minecraft 压缩源/slab/tint 成本。steady 同时记录 manager CPU 和 draw 完成墙钟，draw 包含主机命令资源、提交/等待及完成；不是 GPU timestamp。shader/设备准备、预热、readback 和文件写入不在 draw 内。初始化、编辑、属性写入、BLAS/TLAS和工作集有独立记录；以输出 `input.json`/`samples.jsonl` 的当前边界为准。

性能比较先冻结原始源码/工具/输入和可执行产物，做同版本 A/A，再比较成对样本；保存未提交修改及原始日志，阶段对照不替代原始基线。输入或计时口径不同的结果不作直接收益比较，落在漂移/噪声内的差异保持不确定。临时证据放引擎 `temp/`，不作帧率保证。

无窗口 GPU 功能测试在 [gpu_smoke](crates/renderer/src/gpu_smoke.rs) 和 scene 测试中显式标记 ignored；用单一 filter、`--ignored --nocapture --test-threads=1` 运行。两个 `hardware_pt_rr_` 测试分别启动独立进程。CPU/Java fixture、编译、GPU 功能验证、性能测量和实际游戏验收分别报告。

手工验收包括：进出世界、负坐标与跨 anchor 移动、远近移窗/卸载与回转、放置/破坏、草叶水色、cutout 孔洞、玻璃/水下与局部发光、预算压力、资源重载、SER/RR/画质切换、缩放窗口及退出。观察旧覆盖/失败恢复、图表耗时与日志；动画、实体等未接能力按上述边界检查。编译或离屏通过不能代替游戏验收。
