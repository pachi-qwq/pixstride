[English](../en/stage-d1b1.md) | [简体中文](stage-d1b1.md)

<!-- doc-id: animation-worker; revision: 3; status: merged-history -->

# D1-b1：GIF 后台帧生产流水线

本页保留已合并 D1-b1 的阶段范围和原始测量。当前本地 [D1-b2 分支](stage-d1b2.md)
增加 Renderer 消费并将控制器移交该线程；下文“不播放”的表述描述 D1-b1 阶段，不代表当前分支能力。

本分支增加可独立测试的动画后台。Viewer 仍显示 [D1-a](stage-d1a.md) 的 GIF 静态首帧，
不播放动画、不创建动画 GPU 资源、不改变快捷键，也不安排 Present 截止时间。本轮没有新增依赖。

## 所有权与消费接口

`ImageLoader` 持有一个 `AnimationWorker` 控制器。构造时不创建动画线程或事件，首次
`start(Request)` 才启动一个可复用的 MTA 线程。静态 WIC Worker、身份校验线程、GPU 缓存快速路径、
渲染线程和 Tile Worker 继续承担原有职责。旧 WIC 调用阻塞时，不为新 Session 增建替代线程。

```mermaid
flowchart LR
  Intent[Session 请求] --> Worker[单个惰性 MTA Worker]
  Worker --> Decode[WIC 局部帧]
  Decode --> Canvas[CPU 顺序合成]
  Canvas --> Output[完整 PBGRA8 输出]
  Output --> Consumer[测试消费者：最多两个 Credit]
  Consumer -->|最后一个 Arc 归还 Credit| Worker
```

WIC Factory、Decoder、Frame 和 Converter 都在该 MTA 线程内创建、使用和释放。
Decoder 明确不实现 `Send`。合成画布和保存的局部区域由 Worker 独占。消费者取得
`Arc<FrameOutput>`，其中包含自有像素和只读元数据，不传递 COM 或 `HWND`。

| 标识 | 维护者与生命周期 |
|---|---|
| 文件选择 Generation | 既有文件选择状态维护；动画推进不递增它。 |
| Session ID | 控制器内单调递增，每次 `start` 变化；Session Key 同时包含它和选择 Generation。 |
| 帧序号（Frame Ordinal） | 生产者维护，在同一 Session 的扫描轮次之间持续递增；另有帧索引和轮次。 |
| 源修订（Source Revision） | 已校验的路径、长度、修改时间与 `ImageContent::GifAnimationV1`，区别于 `GifPosterV1`。 |
| 输出凭证（Credit） | 一个输出许可；从解码前申请，到输出最后一个引用释放时归还。 |

`start` 只发布路径和请求，不在调用线程做文件 I/O。请求必须明确指定扫描轮数（或持续生产至取消）
和 Disposal 2 背景策略，两者都不是 Viewer 播放默认值。原始 `delay_cs` 保留百分之一秒单位；
`raw_loop: Option<u16>` 区分缺失、零和正数，不套用 Delay 下限、Loop 播放次数解释或真实播放时钟。
元数据类型依据 [WIC GIF 元数据查询](https://learn.microsoft.com/en-us/windows/desktop/wic/-wic-native-image-format-metadata-queries)。

消费者等待借用的 ready event，再用 `poll(session)` 原子取得状态和至多一帧。等待期间必须保活控制器，
不得关闭其事件句柄。`poll` 拒收其他 Session 的队列输出；已持有旧 `Arc` 的消费者在提交前还须调用
`is_current`，取消、替换或失败都会使其失效。D1-b2 仍需将这项检查与渲染选择、GPU 提交边界协调。

`Starting` 可进入 `Running`、已确认的 `Paused`、`Completed` 或 `Failed`。取消会立即使目标失效，
Worker 清理后才确认 `Idle`。关闭以 `Stopped` 或致命 `Failed` 结束。`Completed` 表示生产完成，
不代表消费者已归还像素。done event 只表示可复用线程退出，不表示单个 Session 完成。
Idle、暂停或等待 Credit 时均使用阻塞等待，不轮询。

## 合成与准入

复用既有有界 GIF Parser：64 MiB 文件及扫描准入、8 KiB 读取缓冲、有界元数据、必要的 Trailer
及有界零填充；仍不校验 LZW 内容。动画模式额外以固定空间收集最大局部帧、恢复区域和原始 Loop。
重复的已识别 Loop 扩展或格式不合法的已识别 Loop 数据会因歧义而拒绝；Poster 路径仍按原规则有界跳过。

每轮画布初始透明。处理当前帧前先执行上一帧的 Disposal：0/1 保留；2 恢复上一帧矩形；
3 恢复其绘制前保存的区域。当前帧为 Disposal 3 时，先保存其区域再绘制。透明索引保留原画布，
不透明调色板像素覆盖原值。输出始终是完整、紧密排列的 PBGRA8 Logical Screen，不是局部矩形。
即使消费者丢弃某帧输出，生产者也不跳过影响后续状态的合成。语义依据 [GIF89a 规范](https://www.w3.org/Graphics/GIF/spec-gif89a.txt)。

Disposal 2 可显式选择清透明，或恢复有效的 Logical Screen 全局背景色（缺失时回退透明）。
两者均有 Fixture 校验，本轮不选择动画最终默认值。Logical Screen 背景、透明索引和查看器棋盘背景是不同概念。
D1-a Poster 的初始透明画布策略保持不变。

## 共享像素预算与背压

普通 CPU 总额度仍为 512 MiB，动画子额度 256 MiB 包含在该总额度内。普通单图准入仍为 256 MiB，
普通 GPU 驻留仍为 512 MiB。Stage C 的独立预算和磁盘格式不变。D1-b1 不分配动画 GPU 资源，
也不启用 128 MiB GPU 动画子额度。

`CpuMemory` 按每份自有像素 `Vec` 的容量记账，静态缓存和动画缓冲共用账本。缓存淘汰只移除成员资格；
渲染器或结果 `Arc` 仍持有像素时，计费继续存在。克隆 Arc 不重复计费，也不会提前归还。
计费字段位于像素字段之后，先释放分配，再归还额度。

设完整画布字节数为 `C`、最大局部帧为 `P`、最大 Disposal 3 保存区域为 `R`。
准入检查 `3C + P + R`：一份画布、两份完整输出、一份局部帧、一份保存区域。尺寸运算检查溢出。
之后每次分配仍须同时通过共享总额度及动画子额度，旧 Session 被外部保留的帧继续计费。
分配或额度不足会返回 Session 错误，Viewer 继续保持 Poster 行为。没有跨 Worker 强制淘汰缓存或反复重试。

队列、消费者、Arc 克隆和正在生产的输出合计最多两个 Credit。先取得 Credit 再解码；
两份都被持有时，生产者通过条件变量等待，Pause、Cancel、Session Replacement 和 Shutdown 均可唤醒。
旧队列的大缓冲由 Worker 在控制锁外退役，调用者不在请求路径同步释放它们。
如果外部仍持有两份旧输出，新 Session 可能等待引用释放，或因内存不足被拒绝。

账本约束应用自有像素容量，不是进程内存硬上限。WIC 内部分配、分配器元数据、线程栈、Parser/控制元数据
和有界诊断样本不计入像素额度，WIC、驱动和 OS 没有进程级硬隔离。

## 取消与关闭边界

暂停采用协作方式：同步调用可能先完成，Worker 确认 `Paused` 后才保证恢复前不再发布输出。
取消检查分布在 WIC 调用之间和合成行之间，不能安全中断已进入的 WIC 调用。

`shutdown` 立即失效输出并唤醒应用等待，不阻塞调用者。Worker 释放 COM 和画布，在锁外清队列，
然后通知 done。`try_join` 只 join 已退出线程。控制器销毁时若 WIC 仍阻塞，最多分离这一个线程；
它持有的 `Arc` 保活事件、账本和数据，且不持有 UI/窗口引用。没有固定退出期限，也不新建替代线程。
D1-b2 必须保持 UI 消息处理，异步等 done 后再 join。外部输出可活得比 Worker 更久，直到最后引用释放才停止计费。

## Windows 验证

本地默认测试通过 87 项，另有 26 项显式诊断默认忽略。动画像素期望值手工指定，独立于合成算法：
80 份输出、960 个像素位置覆盖 Disposal 0–3、局部调色板、透明、两轮扫描和四种原始 Loop。
其他用例覆盖没有 GCE 的 GIF87a、首帧偏移、两种背景、Delay 65535、非法输入、文件修订、预算压力、
最后 Arc、两个 Credit 耗尽、暂停、替换、初始化失败、门控取消及反复 Session。
默认预算测试仅预留额度，不实际分配数百 MiB；以下 Release 诊断使用真实缓冲。

13 组 Release 诊断通过：样本准备、五种动画负载、取消、GIF Poster GPU 回读、Poster Viewer、
长按/反向、普通导航/缓存、合成大 PNG Viewer、Tile GPU 回读。既有静态 GPU 纹理复用通过；
Tile 回读校验 17,425 个像素，包括边缘。大图回归使用自行生成的 20000 × 65 PNG 和独立测试缓存，
未访问私人极限样本或其缓存。窗口诊断通过 Win32 API 执行，不代表新增人工目视验收或动画播放验证。

### 首次 CI 失败与确定性修复

提交 `4631d82a5db95c16fea07cc3f80f3b60d38419c5` 的[首次 Rust 运行](https://github.com/pachi-qwq/pixstride/actions/runs/39782121631)
通过 85 项测试，`animation_failure_discards_queued_frames_and_invalidates_held_output` 失败。
五秒后快照仍为 Running，已生产三份输出、两个未归还 Credit、队列中一帧，其他 WIC 测试通过。
Runner 使用 Windows Server 2025 / Rust 1.99.1，本地使用 Windows / Rust 1.96.0。
日志没有显示 COM 初始化、样本目录冲突或 Node.js 警告是失败原因。

原测试收到两份输出后，按剩余额度快照注入压力。但发布输出不代表 Worker 已析构局部帧。
确定性分配测试表明：快照后释放 16 字节的第二帧局部缓冲和 48 字节输出，会让下一帧的 8 字节局部缓冲
及 48 字节输出都成功分配，因此不能保证进入 Failed。该顺序与 CI 快照一致，但日志没有记录精确指令调度。

修复使用既有 CopyPixels 后门控，将第二帧停在合成前，在像素所有权稳定时填满额度，分别验证
第一份输出仍在队列及已被消费者持有两种情况。队列析构在控制锁外执行，因此等待条件同时检查失败状态
和预期 Credit 数，再断言错误原因、旧输出拒收及最终字节/Credit 归还。没有修改生产代码、timeout、
全局预算或工作流。修复后的目标测试以八个并发进程重复 512 次通过；32 轮完整测试（每轮 16 测试线程、
最多四个并发进程）也全部通过。原测试本地曾连续通过 128 次，说明压力测试通过本身不能证明调度顺序成立。

### 动画负载实测

2026-10-22，Release x64、Rust 1.96.0、windows-rs 0.62.2、系统 WIC，每个负载使用新的测试进程。
样本准备单独运行，未清 OS 文件缓存。各负载均故意停止消费 40 ms，慢消费者之后每帧等待 2 ms。
下表单位为应用侧微秒，各百分位三元组为 P50/P95/P99。

| 负载 | 输出数 | 初始化（单样本） | WIC 局部帧解码 | CPU 合成 | 完整输出准备 | 计费像素峰值（字节） |
|---|---|---|---|---|---|---|
| 4 × 3 确定性样本 | 5 | 4604 | 43/96/96 | 0/0/0 | 0/0/0 | 160 |
| 512 × 256 快消费者 | 128 | 16964 | 693/943/1035 | 57/125/143 | 66/90/154 | 2,097,152 |
| 512 × 256 慢消费者 | 128 | 17211 | 690/1005/1143 | 76/130/196 | 67/95/181 | 2,621,440 |
| 2048 × 1024 大画布 | 4 | 9882 | 10013/10573/10573 | 789/2000/2000 | 1074/1083/1083 | 33,554,432 |
| 512 × 512、64 × 64 局部帧 | 128 | 5990 | 64/101/129 | 2/9/14 | 156/181/206 | 3,178,496 |

解码包括局部分配、元数据/Converter 调用和 CopyPixels；合成包括 Disposal 及区域保存；
输出准备包括分配和画布复制，不含 Credit/暂停等待。零表示低于整数微秒分辨率。
只有四五帧的百分位长尾等同最大值，不是稳定的总体估计。这些样本不能外推全部真实 GIF。

| 负载 | 进程峰值 Working Set（字节） | 最大采样 Private Usage（字节） | 关闭请求到 join（µs，单样本） |
|---|---|---|---|
| 小图 | 11,935,744 | 2,146,304 | 220 |
| 中图快消费 | 14,741,504 | 3,977,216 | 315 |
| 中图慢消费 | 14,897,152 | 4,534,272 | 491 |
| 大画布 | 45,424,640 | 27,389,952 | 342 |
| 局部帧 | 15,233,024 | 5,558,272 | 305 |

进程数据包含 WIC、DLL、分配器和测试框架。Private Usage 只在测试阶段采样，不是连续监测峰值，
也不能单独归因于 WIC。各负载 Credit 峰值均不超过两个，结束后计费字节和未归还 Credit 均为零。
慢消费者 Credit 等待 P50/P95/P99 为 1539/1774/1848 µs，最大值 42701 µs 包含故意停止的 40 ms；
这段停止期间生产量保持为两份输出。

32 次真实 CopyPixels 调用前后区间的取消实验，取消到 Session Idle 为 P50/P95/P99
701/852/1150 µs，最大 1150 µs。区间包含通知和调度唤醒，不能证明每次取消指令都在 COM 内执行。
它测量协作清理，不是线程退出或强制抢占。随后空闲线程关闭到 join 为 360 µs（单样本）。
另有 CopyPixels 返回后的门控实验，验证阶段长时间不返回时的生命周期安全，不将其用作 WIC 耗时。

## 复现与待决策事项

在 Windows 仓库根目录执行，测试目录应位于仓库外：

```powershell
$env:PIXSTRIDE_TEST_ROOT = Join-Path $env:TEMP 'pixstride-d1b1-validation'
cargo test --locked
```

然后准备样本，每个负载使用独立进程：

```powershell
cargo test --release --locked release_animation_prepare -- --ignored --nocapture
cargo test --release --locked release_animation_medium_slow -- --ignored --nocapture
cargo test --release --locked release_animation_cancel_copy_probe -- --ignored --nocapture
```

先准备样本，再运行负载；准备过程不计入测量进程。其他负载筛选名为 `release_animation_small`、
`release_animation_medium_fast`、`release_animation_large` 和 `release_animation_partial`。
日志输出分布、进程计数、像素计费和 Credit。运行 `target/release/pixstride.exe` 打开 GIF 仍只显示静态首帧。

D1-b2 前仍需批准 Delay/Loop 兼容规则、Disposal 2 默认背景、暂停/最小化策略和 GPU 子额度及退役所有权，
接入 ready/done 事件，并在 GPU 提交处复核 Session。广泛编码器兼容性、阻塞存储/codec、Device Lost、
分配失败注入、长期稳定性和真实 Input-to-Photon 尚未验证。本轮不报告播放 FPS、动画 VRAM 或屏显延迟。
Present 返回不代表画面实际显示完成。
