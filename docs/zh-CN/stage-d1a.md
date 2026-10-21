[English](../en/stage-d1a.md) | [简体中文](stage-d1a.md)

<!-- doc-id: gif-poster; revision: 3; status: gui-review -->

# D1-a：GIF 静态首帧

本分支为基本 GIF87a/GIF89a 文件实现静态首帧图（poster）：将第一个图像帧按偏移合成到逻辑画布
（Logical Screen），后续帧不改变结果。本轮不实现动画、播放时钟、Loop 策略、暂停操作或动画缓存预留。
EXIF 5/7 修复已通过 [PR #6](https://github.com/pachi-qwq/pixstride/pull/6) 合并，本分支也已包含该修复。

## 解码与缓存契约

现有后台 WIC Worker 在打开 WIC 前预检 GIF 结构，检查签名、画布、颜色表、帧矩形、扩展、
Sub-block 长度、块结束标记及 Trailer。扫描使用 8 KiB 读取缓冲和固定大小的元数据存储，
在读取和块处理之间检查取消。预检不实现 LZW，不能保证发现损坏的像素编码。

Trailer 仍然必须存在，其后只允许连续零填充，且整个文件和扫描仍受原有 64 MiB 上限约束。
扫描使用固定 8 KiB 缓冲，逐块检查内容，并在读取之间检查取消；短读不视为文件结束。
非零尾随数据继续拒绝。这项兼容规则没有放宽颜色表、Extension、Sub-block 或结束结构检查。

WIC 提供第一个局部帧及 PBGRA8 像素。解码器核对 WIC 的画布和矩形信息，然后分配一份清零的
Logical Screen 画布，将局部像素写入对应位置。满画布帧只调用一次 CopyPixels；带偏移的帧逐行直接写入画布，
避免额外分配一份完整局部位图。帧外仍保持透明。静态首帧没有后续合成，因此不在输出后执行 Disposal。

默认背景策略始终是透明画布，包括文件具有有效全局背景颜色的情况。帧内不透明像素保留调色板颜色，
透明索引保持透明。确定性测试覆盖绿色全局背景、局部不透明黑色像素、局部颜色表及没有全局表的文件。
本轮未运行其他查看器对照；这是明确的产品策略，不代表所有查看器都会选择相同背景。

SourceIdentity 新增固定输出语义 ImageContent::GifPosterV1。CPU/GPU 缓存键同时包含该语义及来源路径、
长度和修改时间，发布前会复核来源。常见替换会使缓存失效；保留相同元数据的原位改写仍可能漏检。
预检至 WIC 解码结束期间保留只读句柄，拒绝新建写入或删除句柄。

继续复用 ImageLoader、GPU 快速路径、预加载、Generation 和同方向长按进度显示。
GPU 命中不进入 WIC；CPU 命中跳过预检和解码，仅在 GPU 缓存缺失时上传纹理。
渲染线程拥有纹理和 Immediate Context。错误沿用标题栏及 stderr 提示，用户仍可选择其他文件。

## 准入与资源

| 资源 | 限制或行为 |
|---|---|
| Logical Screen 解码像素 | 256 MiB；单边不超过 16,384 |
| 普通 CPU / GPU 缓存 | 各 512 MiB，与普通图片共用 |
| GIF 文件扫描 | 64 MiB，完整结构须在此范围内 |
| 元数据 / 帧数 / 块数 | 4 MiB / 10,000 / 1,000,000 |
| 应用像素存储 | 一份 Logical Screen 画布，不保存额外完整局部位图或帧列表 |

这些是准入策略，不是 GIF 格式上限。本轮拒绝 Plain Text Extension、Trailer 后的非零数据、未知块标记、
非法保留字段及不支持的元数据。Comment 和 Application Extension 只做有界跳过，其 Loop 内容不建立播放语义。
伪装为其他已支持扩展名的 GIF 内容会被拒绝，防止绕过预检。

应用无法硬限制 WIC 内部分配，也无法安全抢占正在阻塞的文件读取或 WIC 调用；取消在下一个检查点生效。
UI 与渲染线程继续独立运行。超限 GIF 不进入 PNG Tile Builder。全局预算没有提高，
本轮也没有建立 D1-b 的动画 CPU/GPU 子额度和 Credit。

## 验证及数据边界

生产解码测试使用手写坐标参考比较全部像素，覆盖多帧输入、局部颜色表、透明、GIF87a、无全局表及常见来源修订。
测试拒绝小型 Fixture 的每个截断前缀、Prototype 中的截断像素反例、非法元数据、扫描和元数据超限及超预算画布。
同时测试取消和只读锁定，没有使用私人图片或超大 PNG 缓存。

填充回归覆盖无填充、167 字节零填充、多缓冲块填充、短读、非零和混合尾随数据、缺少 Trailer、
Sub-block 截断、取消及恰好 64 MiB 的边界。生产 WIC 测试比较有无填充的像素结果，并验证来源修订失效。
GPU 回读和 Viewer 诊断也使用自行生成、带 167 字节填充的 GIF。

显式 Windows Release 诊断覆盖真实 GPU 回读、SRV 复用、CPU 命中后不重复预检、GIF/JPEG/PNG/BMP 导航、
新 GPU 命中后旧解码结果被丢弃、文件修订、错误恢复、Fit/100%、Resize、最大化/最小化/恢复，以及存在待完成 GIF 时关闭。
长按诊断注入了 80 ms Worker 延迟，用于检查进度、反向和松键语义，不代表实际 GIF 解码速度。
这些是自动化窗口/API 检查，不代替人工目视验收。

一次 Windows x64 Release 测试分别解码两份自行生成的 2048×1024 文件，各 32 次，没有清空 OS 文件缓存。
满画布解码 P50/P95/P99 为 10.867/11.630/17.052 ms，局部帧为 9.937/11.520/11.650 ms。
每份输出拥有 8 MiB 画布。整个进程 Working Set 峰值为 20,209,664 字节，采样最大 PrivateUsage 为 10,498,048 字节。
这些数值包括 WIC、DLL、allocator 及测试进程开销，不能作为 WIC 专属分配估算或通用硬上限。
GPU 诊断保留的纹理有效载荷为 8,388,656 字节，即一份 8 MiB 首帧和一份 48 字节首帧，并验证了 SRV 复用。
载荷计费不包括驱动分配粒度及其他 GPU 资源。

满画布文件含两个图像帧并先运行，局部帧文件含一个图像帧；首轮还包含 WIC codec 的初次初始化。
这些数据分别描述两种负载，不能据此比较整帧复制和逐行复制策略的快慢。

Present 返回、解码完成及应用内部计时均不等于 Input-to-Photon。
Stage C 合成图集成诊断使用独立缓存另行执行；本轮不测试私人极限 PNG 或长期稳定性。

## 构建和人工验收

将 PIXSTRIDE_TEST_ROOT 指向仓库外专用可写目录，然后运行：

```powershell
cargo build --release --locked
cargo test --release --locked release_prepare_gif_poster_samples -- --ignored --nocapture
& .\target\release\pixstride.exe (Join-Path $env:PIXSTRIDE_TEST_ROOT 'gif-poster-gui\partial-transparent.gif')
```

批准前请在真实桌面检查：

1. 局部透明首帧的矩形外显示查看器背景，没有填充文件调色板中的绿色。对照 partial-opaque.gif，确认帧内黑色像素不透明。
2. 与生成的 JPEG/PNG/BMP 往返浏览，快速长按、反向和松键，确认最终选择正确。GIF 应始终静止。
3. 操作 Ctrl+O、F（Fit）、1（100%）、Resize、最大化/最小化/恢复和 Alt+Tab。
4. 打开 broken.gif 后继续浏览有效文件，再用 Alt+F4 退出。加载 memory-full.gif 时重复打开和关闭，确认没有残留进程。
5. 对比 padded-transparent.gif 与 partial-transparent.gif，首帧内容应一致。重新打开本地曾因零填充被拒绝的 GIF，
   并记录仍存在的兼容性问题。GIF 仍应保持静止，当前尚未实现动画播放。

实现位置：[GIF 预检及合成](../../src/image/gif.rs)、[像素 Fixture](../../src/image/gif_tests.rs)、
[Viewer 诊断](../../src/app/gif_tests.rs)。块语义依据 [GIF89a](https://www.w3.org/Graphics/GIF/spec-gif89a.txt)，
WIC 元数据使用[官方查询路径](https://learn.microsoft.com/en-us/windows/win32/wic/-wic-native-image-format-metadata-queries)。
