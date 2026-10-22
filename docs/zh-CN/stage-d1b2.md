[English](../en/stage-d1b2.md) | [简体中文](stage-d1b2.md)

<!-- doc-id: animation-playback; revision: 2; status: animation-playback-review -->

# D1-b2：在 D3D11 渲染线程播放 GIF

本地分支将已合并的 [D1-b1 生产者](stage-d1b1.md)接入 Viewer。多帧 GIF 先显示静态 Poster，再播放动画；
单帧 GIF 保持 Poster 路径。Windows 自动化检查通过，负责人报告 Windows 人工验收 8/8 项通过。
代码评审和 PR 合并尚未完成。
项目没有公开 Release，本轮没有新增依赖。

## 所有权与交接

`ImageLoader::take_animation` 将唯一惰性控制器移交现有 Render Thread。可复用的单个 MTA Worker
仍独占所有 WIC COM 对象、画布、局部帧和 Restore Previous 数据。跨线程只传递拥有型 PBGRA8 像素和不可变元数据。
JPEG/PNG/BMP 和 Stage C Tile Worker 保持原有执行模型。

Renderer 只为最终稳定选择激活动画，分别核对文件选择 Generation、Source Revision、Session ID、
`GifAnimationV1` 内容类型和 Frame Ordinal。长按导航的每张进度 GIF 不会反复启动 Session。
取消立即失效旧输出，上传前及 Present 提交前再次核对。已经交给 DXGI 的命令无法撤回。
同一 GIF 被重新选择时，先退役旧动画资源，再复用 Poster。

序号 0 输出提供原始元数据，并核对 Poster 的源修订；其像素不重复上传。
Poster 的成功 Present 返回边界作为首帧 Delay 的起点。Renderer 最多保留一个未来 `Arc<FrameOutput>`，
生产者可使用另一个 Credit。排队、生产中和消费端持有的输出合计仍最多占两个 Credit，
没有第二个上传队列。最后一个 Arc 释放时，才归还 CPU 字节和 Credit。

## GPU 资源与预算边界

每份被接受的完整 PBGRA8 画布更新同一个 DEFAULT BGRA8 Texture2D/SRV。
动画帧不进入静态 GPU LRU。Immediate Context 及全部 GPU 资源仍归 Render Thread。
`UpdateSubresource` 返回后可以释放 CPU 帧，因为 API 已复制源数据。同一 Context 的命令顺序处理 Draw/update 依赖；
帧延迟事件不充当完成 Fence。资源竞争可能引起驱动内部临时复制，具体见微软的
[UpdateSubresource 契约](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-updatesubresource)。

替换时，Renderer 解除 SRV 绑定，在最后一次使用之后插入 EVENT Query。
旧纹理及其计费一直保留到 `GetData` 返回真实完成值；只检查 `Ok` 不够，因为 S_FALSE 也可能映射为 `Ok`。
只在处理实际工作时回收，不为空闲 Query 轮询，依据是 [EVENT Query 契约](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_query)。
含退役资源在内，最多保留两个动画纹理对象。
有限动画结束后，保留最后纹理，直到切图或退出。

| 资源 | 限制与计费 |
|---|---|
| 普通单图 | 256 MiB，不变。 |
| CPU 像素 | 共同 512 MiB 账本；动画最多使用其中 256 MiB，含画布、局部帧、保存区域和两个输出。 |
| GPU 图片载荷 | 共同 512 MiB；动画活动/退役载荷最多使用其中 128 MiB。静态准入计入动画字节，分配前淘汰可释放、未固定的条目。 |
| Stage C | 原有独立 CPU/GPU/磁盘预算及缓存格式不变。 |

动画额度不足时，停止该 Session，提供非模态提示，保留 Poster 或最后有效帧，不无限重试。
载荷计费不能硬限制 Working Set、WIC 分配、D3D11 驱动复制、交换链缓冲或物理显存。
静态 LRU 退役仍沿用既有 D3D11 runtime 语义；新增动画退役明确计费到 Query 完成。

## 时钟与兼容策略

Render Thread 维护可用虚拟时间测试的单调时钟。等待集依次包含工作、可消费的生产者输出、
单次 Waitable Timer，以及需要绘制时的 DXGI 帧可用事件。没有逐帧线程或 Sleep 轮询。
正常截止时间沿前次 Deadline 推进，避免唤醒抖动逐帧累加。若已经落后一整段新的展示时间，
或发生解码 Underflow，则重新起算，不连续提交大量帧追赶。

下一帧未就绪时保留当前画面。Underflow 只统计一次，随后移除计时器，由新数据或用户/窗口工作唤醒。
顺序合成不跳过 Disposal 依赖。本实现不丢弃已解码帧来追赶固定墙钟位置，持续慢解码会减慢播放。
因此跳过显示计数按设计为零，与过期 Session 拒收分别统计。负责人已验收本次构建的短 Delay 节奏。

| 原始元数据 | 本分支展示策略 |
|---|---|
| Delay 为 0 或 1 个百分之一秒 | 100 ms。 |
| Delay 为 2 / 3 / 10 个百分之一秒 | 20 / 30 / 100 ms。 |
| 没有 Loop 扩展 | 播放一轮。 |
| Loop = 0 | 重复直到取消。 |
| Loop = 正整数 n | 首次播放后再重复 n 轮。 |

GIF89a 将非零 Delay 定义为百分之一秒，但这本身并未规定零延迟兼容规则。
此策略依据独立检查的 [Chromium Delay 处理](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/platform/graphics/deferred_image_decoder.cc)
和 [Firefox FrameTimeout](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/image/FrameTimeout.h)。
Loop 依据 Firefox 的 [NETSCAPE 解析](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/image/decoders/nsGIFDecoder2.cpp)
及确定性浏览器样本核对。Worker 原始元数据不变。负责人已在人工测试中验收当前短 Delay 节奏和 Loop 行为，
代码评审仍待完成；没有采用此前 Prototype 建议的 20 ms 下限。这些检查不代表兼容所有编码器。

2026-10-22，隔离的 Headless Edge 155.0.4283.62 和 Firefox 153.5.0 分别播放自制三色 GIF 4.5 秒。
宿主时间戳截图读取合成器像素，不测量光子。Firefox 取得 755 个样本，截图调用 P50 为 1.502 ms；
原始 0/10/20/30/100 ms 的颜色变化间隔中位数为 97.33/97.33/18.06/28.68/97.33 ms。
Edge 取得 77 个样本，调用 P50 为 62.196 ms，无法用其欠采样的 20/30 ms 场景确认精确时长；
0/10/100 ms 场景表现一致。两者都观察到无 Loop、1、2 的样本在一、二、三轮后停止，0 则持续播放。
这些是兼容性证据，不是浏览器性能对比，也不保证 Pixstride 的实际屏幕节奏相同。没有复制外部解码器源码。

最后一帧从本次提交起保留完整归一化 Delay，有限播放结束后取消后续截止时间并保留画面。
消费端取消前，生产者可能在两个 Credit 的上限内预先准备后续输出；每轮开始都会重置合成画布。

## 暂停与生命周期

Space 只切换活动动画的用户播放意图，忽略按键重复。最小化和原生文件对话框临时暂停，
恢复窗口或取消对话框不会解除已有的手动暂停。失焦继续播放，同时仍会释放长按导航状态。
暂停保存当前帧剩余时间，恢复不累计暂停期间的时间。已经接受的上传可能在暂停确认前完成提交边界，
不会因此建立周期循环。

切换文件先失效旧 Session，旧帧不能重新取得提交资格。Session、解码、计时器创建及纹理准入错误为非模态提示。
Device/Present 错误沿既有安全退出路径处理，完整 Device Lost 恢复仍不在本轮范围。

退出时，Renderer 取消生产、释放持有帧，在自己的线程释放 GPU 资源，再将仅含 CPU 控制状态的控制器交回 UI 清理。
UI 继续处理消息并异步等待 Worker 完成事件，COM、画布、队列清理之后才 join。
CopyPixels 后的门控测试证明 Worker 被阻塞时 Render 仍能退出。Credit 和暂停等待可取消。
WIC 同步调用不能强制返回，原有 2 秒关闭宽限到期后，仍可能为进程退出安全分离唯一 Worker，
这不构成固定的 WIC 取消时限保证。

## 自动化证据与测量

默认测试通过 95 项，另有 31 项显式诊断不在默认运行中。播放新增虚拟时钟、Session/顺序、暂停意图和非法状态测试。
Release 测试使用真实 WIC 和硬件 D3D11。十次 GPU 上传复用一个 Texture，跨两轮的 120 个像素位置
与独立坐标参考完全一致，含透明和 Disposal 0–3。测试覆盖 Credit/字节回收、共同额度拒绝、Query 退役、
有限循环、暂停/对话框/最小化、旧结果处理、慢 Worker Underflow、静态导航及关闭。

二十组 Release 诊断通过，包含样本准备和五种播放负载。原有 Poster、EXIF GPU 回读、JPEG/PNG/BMP 缓存/导航、
长按反向、合成 20000 × 65 PNG Viewer 和 17,425 像素 Tile 回读均通过。
二十次受控静态 GPU 命中没有新增 WIC CopyPixels。在有确认状态门控的 220 ms 观测窗口中，
有限结束、暂停及静态空闲都没有额外唤醒或 Present 提交。这些窗口不能证明长期空闲行为。另以正式 Release 可执行文件独立运行三次，在播放中、暂停后和最小化后
发送 WM_CLOSE，进程退出分别耗时 24.122/29.049/25.651 ms，各一个样本；最终动画字节/Credit 为零，未使用关闭分离。
本轮未访问私人极限 PNG 或其缓存。

长按复测暴露出批次交接缺陷：`take_pending` 之后松键，可能让旧进度图片过期，
但同批次更新的 GPU 探测仍然有效。原来跳过整个批次的 `continue` 丢掉探测，UI 等不到 Miss，最终解码无法入队。
修复只丢弃过期图片，图片上传失败后也保留后续工作。门控测试修复前失败、修复后通过；
四个进程共重复 64 次门控、8 次长按/反向测试，全部通过，没有修改 timeout 或削弱断言。
UI 播放入口与 GPU 画面也按当前选择 Generation 校验。

<!-- measurements-start -->

计时单位为微秒，三元组为 P50/P95/P99。

| 负载 | Decode n | WIC decode | CPU compose | Output copy | GPU upload | Deadline Lateness |
|---|---|---|---|---|---|---|
| 4 × 3 / 5 frames | 63 | 84/125/144 | 0/0/1 | 0/0/1 | 8/94/152 | 7825/15157/15372 |
| 512 × 256 / 128 frames | 63 | 806/1077/1148 | 64/142/204 | 66/82/120 | 29/39/263 | 7887/15221/16001 |
| 2048 × 1024 / 4 frames | 6 | 9944/10503/10503 | 956/1984/1984 | 1038/1057/1057 | 854/979/979 | 7621/11119/11119 |
| 512 × 512 / 128 local frames | 63 | 123/194/224 | 5/13/21 | 190/228/325 | 48/77/324 | 7257/14622/15011 |
| 512 × 256 / short delays | 63 | 812/1161/1274 | 71/140/183 | 70/104/208 | 30/51/336 | 7555/14543/15372 |

| 负载 | Shared CPU peak bytes | Animation CPU peak bytes | Animation GPU peak bytes | Process peak WS bytes | Max sampled Private Usage bytes |
|---|---|---|---|---|---|
| 4 × 3 / 5 frames | 240 | 192 | 48 | 49106944 | 60194816 |
| 512 × 256 / 128 frames | 3145728 | 2621440 | 524288 | 54317056 | 65425408 |
| 2048 × 1024 / 4 frames | 50331648 | 41943040 | 8388608 | 114491392 | 110608384 |
| 512 × 512 / 128 local frames | 4227072 | 3178496 | 1048576 | 56692736 | 69865472 |
| 512 × 256 / short delays | 3145728 | 2621440 | 524288 | 54714368 | 66895872 |

| 负载 | Selection → CPU output (n=1) | Selection → updated Present (n=1) | CPU output → updated Present (n=1) | Present P50/P95/P99 | Shutdown → cleanup observed (n=1) |
|---|---|---|---|---|---|
| 4 × 3 / 5 frames | 162871 | 270900 | 108028 | 67/93/798 | 12108 |
| 512 × 256 / 128 frames | 186594 | 280707 | 94113 | 64/95/5882 | 13395 |
| 2048 × 1024 / 4 frames | 210391 | 300209 | 89818 | 126/878/878 | 14268 |
| 512 × 512 / 128 local frames | 169299 | 276212 | 106913 | 69/101/730 | 15643 |
| 512 × 256 / short delays | 186211 | 281486 | 95275 | 60/99/8066 | 16755 |

<!-- measurements-end -->

测量日期为 2026-10-22–23。上述计时来自 Release x64、Rust 1.96.0、windows-rs 0.62.2、系统 WIC，以及 RTX 4080 SUPER / Feature Level 11.1。
先单独准备样本，各负载在独立进程测量，不清理 OS 文件缓存。小图、中图、局部帧和短延迟场景在 60 次更新帧 Present 后停止；
大画布有限样本只有三次更新。解码计数可能包含有界的预先生产或取消工作。小样本 P99 只是最大值，不代表总体分位数。
Private Usage 只在阶段采样，不是连续峰值；进程计数包含驱动、DLL、allocator 和测试框架。
每项完成后的诊断均无未归还动画字节或 Credit。性能测试的关闭确认包含此前的 Render/静态 Worker 清理，
因此它是仅动画 Worker 完成时间的上界。生产者与 Renderer 两份计时报告的统计范围不同。

Deadline Lateness 在上传前结束计时，Present 耗时及选择到 Present 则在 API 返回时结束。
普通计时器粒度与调度会影响应用延迟，本轮没有因果跟踪来单独归因。Present 返回、UpdateSubresource 返回和 DXGI 帧可用事件
具有不同语义，均不测量 Input-to-Photon；后者需要 PresentMon/ETW 或外部显示测量。
D1-b1 数据只提供背景，不能当作受控的前后对照。

### 普通图片对照

以未修改的 detached `1073e09622e39c14e33edb7d833c7ad6824228ad` 工作树与当前分支，
交替运行三组独立进程导航/缓存诊断。双方在本地存储生成相同的三张 256 × 256 BMP。
当前诊断使用生产动画控制器交接，断言普通图没有启动动画，并保留 GPU 命中不新增 WIC 的断言。
每次包含 100 次顺序命中及原诊断的快速导航尾部，六次均记录 110 个命中到 Present 样本；没有清理 OS 缓存。

| Build | Run | n | P50 µs | P95 µs | P99 µs |
|---|---|---|---|---|---|
| baseline | 1 | 110 | 268 | 6565 | 7079 |
| playback | 1 | 110 | 262 | 6358 | 6801 |
| baseline | 2 | 110 | 264 | 6033 | 6863 |
| playback | 2 | 110 | 269 | 6206 | 6660 |
| baseline | 3 | 110 | 288 | 6295 | 6770 |
| playback | 3 | 110 | 307 | 6067 | 6533 |

这组小型受控对照未见明显静态路径回归：基线 P50 为 264–288 µs，当前为 262–307 µs，P95 范围重叠。
它不能证明性能提升，也不是总体性能保证。终点仍是 Present 返回，不是屏幕显示完成。

## 复现与评审

将 `PIXSTRIDE_TEST_ROOT` 设置为仓库外独立本地目录，准备自制样本，再分别运行诊断：

```powershell
cargo test --release --locked release_animation_prepare -- --ignored --nocapture
cargo test --release --locked release_animation_gpu_frames -- --ignored --nocapture
cargo test --release --locked release_gif_playback_viewer -- --ignored --nocapture
cargo test --release --locked release_animation_stale_underflow -- --ignored --nocapture
$env:PIXSTRIDE_PLAYBACK_CASE = 'medium'
cargo test --release --locked release_gif_playback_profile -- --ignored --nocapture
cargo build --release --locked
& '.\target\release\pixstride.exe' (Join-Path $env:PIXSTRIDE_TEST_ROOT 'animation-fixtures\medium.gif')
```

其他已准备的播放场景为 `small`、`large`、`partial`。已准备的 `short` 样本将自制 GCE Delay 设为 0/1/2/3/10 个百分之一秒，没有使用外部 GIF。
`loop-once`、`loop-twice`、`loop-thrice` 可用于人工有限循环检查。构建和测试日志输出分布、进程计数及最终计费。

### 负责人提供的 Windows 人工验收

项目负责人报告，已在真实 Windows 桌面完成下列 8/8 项验收，全部通过。验收代码提交为
`f7ad89465c4e3245bf70faa5a841b71c18c327a0`，后续验收记录提交只修改文档和配对元数据。
这些是负责人提供的功能及目视观察，不构成像素级正确性证明、精确计时或长期 CPU 占用保证。

| 验收项目 | 负责人提供的结果 |
|---|---|
| 正常 GIF 播放。 | 通过。 |
| 短 Delay 节奏。 | 通过。 |
| 无 Loop、有限 Loop、无限 Loop。 | 通过。 |
| Space 暂停/恢复。 | 通过。 |
| 最小化、恢复、Alt+Tab、Ctrl+O。 | 通过。 |
| GIF 与 JPEG/PNG/BMP、合成大 PNG 往返。 | 通过。 |
| 长按、反向、Resize、Fit/100%、播放中关闭。 | 通过。 |
| 暂停、播放结束、静态空闲时未观察到持续异常占用。 | 通过。 |

上述自动化证据和性能测量边界与人工验收记录分别保留。代码评审和 PR 合并仍待完成。

广泛编码器兼容性、User Input Flag 语义、阻塞存储/codec、Device Lost、真实分配失败、长期稳定性及 Input-to-Photon
仍未全面验证。不支持 Seek、调速、倒放、APNG/WebP 动画，本轮没有 HDR/ICC 或 Stage C 缓存改动。
GIF 文件准入仍为 64 MiB，结构预检不能保证 LZW 正确性。
Poster/动画画布和 Disposal 2 使用已显式测试的透明清除策略，全局背景与查看器背景仍是不同概念。
人工验收不能证明所有真实 GIF 编码器的背景或时序行为均一致。
