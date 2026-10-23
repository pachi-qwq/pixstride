[English](../en/stage-d3a1.md) | [简体中文](stage-d3a1.md)

<!-- doc-id: view-transform; revision: 1; status: view-transform-review -->

# D3-a1：统一缩放、平移与视口几何

本分支让普通图片、GIF Poster/动画帧及超大 PNG Tile 使用相同视口几何。
负责人 Windows 人工验收和 PR 审核待完成。D1-b2 播放已经合并，本轮改变其几何，不改变播放时钟或解码器。

## 原有缺陷与修改边界

基线 `751023f06d255427614db837fb68f227e80676a4` 的 `RenderSnapshot` 已携带 `zoom` 和 `pan`，
但普通 `Renderer::draw` 调用 `fit_viewport` 时只传尺寸和模式，居中 Viewport 忽略这两个字段。
GIF 播放也使用普通绘制路径。相对地，`large::view::pieces` 通过另一套倍率/偏移公式消费它们。
滚轮输入已经保留小数 Delta，但未读取鼠标位置。

`src/view_transform.rs` 现在集中维护纯 f64 `ViewTransform`。普通 Renderer 复用既有 Tile Quad Shader，
增加一个 32 字节常量缓冲，并使用覆盖客户区的 D3D11 Viewport。先裁剪图像矩形和对应 UV，再转换为 f32 NDC，
避免将负 Pan 或巨大图像坐标作为非法 D3D11 Viewport。Tile 选块共用变换，保留原有 LOD、边界像素、
低分辨率回退、有界请求和磁盘格式。

## 坐标与视图规则

原图坐标描述像素边界：`[0, width] × [0, height]`，像素中心为 `(x + 0.5, y + 0.5)`。
客户区坐标使用物理像素。EXIF 使用解码后已应用方向的尺寸，GIF 使用 Logical Screen，Tile 始终使用原图尺寸。

| 状态 | 规则 |
|---|---|
| Fit 基础倍率 | `min(client_width / image_width, client_height / image_height)`；完整放入窗口，也允许放大小图。 |
| NativePixels 基础倍率 | 1.0，与显示器 DPI 无关。重置时边界对齐整数像素，奇数像素余量留在右/下方。 |
| 有效倍率 | 基础倍率乘额外 Zoom；Zoom 仍为 0.01–64，Fit 本身不受此范围限制。 |
| Pan | 缩放后以客户区物理像素平移。不按图像边缘 Clamp，F/1 可重新居中；±10^15 仅为防止无限累加的数值保护。 |
| 非法几何 | 零尺寸、非有限值和非法 Zoom 返回 `None`，绘制/Tile 选块跳过无效几何。 |
| F / 1 | 选择模式，重置额外 Zoom/Pan，并结束当前拖动。 |
| 新图片 | 保留模式，在接受新图片/预览时重置额外 Zoom/Pan；加载期间旧图保留原视图。 |
| GIF 换帧/Loop | 保留视图、文件选择 Generation 和 Session，几何输入不重启播放。 |
| Resize | 重新计算 Fit 基础倍率，把旧客户区中心下的原图位置保持在新中心。 |
| 最小化 | 保留最后非零客户区尺寸；恢复同一尺寸时精确保留 Pan。 |

变换关系为 `p = offset + u × scale`。滚轮缩放先求 `u = (p − offset) / scale`，
再计算 `new_offset = p − u × new_scale`。Pan 使用实际限幅后的新倍率，达到倍率上下限后继续滚动不会移动图像。
鼠标位于留白处时也使用同一规则。锚点缩放后的小数 Pan 可能使像素边界落在客户区像素之间；按 `1` 可恢复整数对齐，
一个源像素对应一个客户区物理像素。

Win32 滚轮提供有符号屏幕坐标，`window.rs` 对两个分量作符号扩展，经 `ScreenToClient` 转换，
再应用 `1.2^(Delta/120)`，不丢弃小于 120 的 Delta。平移仍使用左键拖动。
Capture 丢失、失焦、取消、DPI 迁移、F/1、切图或关闭均清除拖动起点。API 依据见 Microsoft
[WM_MOUSEWHEEL](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-mousewheel) 与
[ScreenToClient](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-screentoclient)。

## 状态与资源所有权

`Shared::activate_view` 在既有 pending-state Mutex 内确认接受的 Generation 和最终尺寸，
复用导航水位与进度区间拒收旧候选，并返回最新快照，避免上传后恢复旧输入。交接只有尺寸，UI 数学不读取文件，也不接收 COM 资源。
GIF 换帧不调用此方法。静态 GPU 命中走同一入口，不重新解码或上传图片。

D3D11 对象仍由现有 Render Thread 独占。视图变化只更新 Quad 常量，不替换当前 SRV，不增加 Worker、计时器或渲染循环。
普通单图仍为 256 MiB，CPU/GPU 共享额度仍各为 512 MiB；动画 CPU 256 MiB、GPU 128 MiB 子额度仍在总额内。
Stage C 预算及缓存格式不变。固定 32 字节管线缓冲不属于图片载荷。

## 验证证据

自动化使用生成的可公开安全样本和真实 Windows WIC/D3D11 资源，未读取私人极限 PNG 或其缓存。
隐藏窗口测试不等于人工验收。

| 检查 | 证据 |
|---|---|
| 纯几何 | Fit/100%、EXIF 后非正方形尺寸、四角、Pan、图内/图外锚点、小数 Delta、限幅、Resize 及 2,000 次缩放往返，坐标容差 10^-7 像素。 |
| 选择状态 | 同图确认保留视图，新接受的选择/进度图片重置视图，旧 Generation 无权重置新几何。最小化/恢复及物理客户区尺寸变化保持观察中心。 |
| GPU 几何 | 独立双线性/Alpha 公式核对 7,869 个 Render Target 像素，覆盖六种普通视图、两轮共十个 WIC 合成 GIF 帧；RGBA8 通道容差为 2。SRV 身份与纹理载荷不变。 |
| Win32 联动 | 负屏幕坐标、Delta 15、左键拖动及 Capture/失焦取消、六次普通图/GIF/Tile 选择、GIF Loop/暂停/恢复、最小化/对话框标记及 120 次变换。变换期间新增 WIC CopyPixels 和图片纹理上传均为零；门控 220 ms 静态空闲观察期新增 Present 和渲染唤醒均为零。 |
| 既有路径 | 选定的 17 项 Release 探针全部通过，覆盖 EXIF、GIF Poster/播放/Delay/Loop/Credit、过期结果、缓存、导航、合成大 PNG、Tile 接缝和关闭；Tile GPU 回读另核对 17,425 个像素。 |

首轮 GPU 探针误用请求的测试宽度解释初始 HWND 尺寸的 Back Buffer。明确同步缓冲尺寸后通过，像素参考和容差未变。
这是探针前置条件错误，不能据此归因于驱动。

长按回归还在本分支和未修改基线上发现已有呈现等待缺陷。超时快照显示 Generation 31 已接受并上传，最后 Present 却停在 25–27。
候选在取走 DXGI 帧就绪通知后失效时，跳过 Present 会丢掉这次准入，下一张可能无限等待。
现在 Render Thread 保留一次已取得的准入，直到 Present；工作通知仍优先，空闲仍无限等待。
确定性无信号事件测试及连续 20 轮慢解码长按均通过，未扩大超时。此准入不是 GPU Fence，API 依据见
[GetFrameLatencyWaitableObject](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgiswapchain2-getframelatencywaitableobject)。

## Release 受控测量

同一机器（RTX 4080 SUPER、Feature Level 11.1、Rust 1.96.0）对每类负载交替运行三轮基线与分支进程，
均使用 `--release --locked`、相同生成样本和输入序列。下表为每轮 P50/P95 的范围，不是合并样本后的分位数。
未隔离 OS/驱动缓存和桌面调度。

| 负载 / 指标 | 每轮样本数 | 基线 P50 / P95（ms） | 分支 P50 / P95（ms） |
|---|---|---|---|
| 静态 GPU 命中，选择 → Present 返回 | 110 | 0.300–0.322 / 5.787–5.917 | 0.279–0.342 / 6.311–6.816 |
| 静态 CPU 命中，选择 → Present 返回 | 100 | 0.480–0.491 / 5.380–6.694 | 0.440–0.518 / 5.383–6.714 |
| 局部帧 GIF Deadline Lateness | 60 | 7.401–8.112 / 14.279–14.673 | 7.385–8.208 / 14.241–15.016 |
| 中等 GIF Deadline Lateness | 60 | 6.270–7.983 / 14.268–14.803 | 7.107–7.836 / 14.649–14.789 |
| 局部帧 GIF GPU 上传提交 | 60 | 0.049 / 0.067–0.072 | 0.049–0.053 / 0.063–0.083 |

静态 GPU 命中 P95 在对应轮次中高出 0.394–0.905 ms，三轮不足以确定原因。静态渲染命令准备 P95 从 6–8 增至 8–10 µs。
缓存命中断言仍证明没有重复 WIC 解码。GIF 专项无 Underflow/失败，输出和资源保持有界。
这些结果提供功能回归证据，不能据此声称延迟优化。

独立的 8193 × 4097 Tile 探针两轮访问五个位置；五次暖 GPU 命中视口完成 P50/P95 为 113/123 µs，
没有额外 Tile 读取或上传。Tile CPU 驻留峰值 63,197,532 字节，GPU 载荷 93,206,000 字节，均在既有限制内。
本次包含冷构建，不能与历史 Stage C 数据作严格前后对照。

## 待验收与限制

负责人需完成以下十项 Windows 检查后，才能将 D3-a1 标为人工验收通过：

1. JPEG、PNG、BMP 的滚轮缩放及左键拖动。
2. 图像细节在连续放大/缩小时保持在鼠标下方，包含留白锚点。
3. F/1 往返，100%、150%、200% DPI 下检查 100% 像素对应。
4. 切换不同尺寸/宽高比图片，保留模式，接受新图时重置额外视图。
5. GIF 播放中缩放/平移，Logical Screen 保持稳定。
6. GIF 暂停/恢复及 Loop 后视图不跳变。
7. 使用生成的 Tile PNG，检查 Fit/100% 和其他倍率下四角、中心及接缝。
8. Resize、最小化/恢复、Alt+Tab，以及不同显示器/DPI 间移动。
9. 长按/反向、Ctrl+O/取消、拖动时丢失 Capture、加载中关闭。
10. 静态空闲无持续异常 CPU 占用和周期刷新。

真实多显示器/DPI 迁移、极端物理输入设备及长期稳定性仍未验证。自由 Pan 可将图像完全移出窗口，F/1 可恢复。
WIC 取消、Device Lost 恢复、资源耗尽、Stage C 首次构建及视口长尾限制仍保留。
计时均为应用侧事件，Present 返回不代表屏幕显示完成或 Input-to-Photon。

`release_view_transform_gpu_pixels` 和 `release_view_transform_input_and_playback` 通过
`PIXSTRIDE_TEST_ROOT` 存放生成输入。复测命令和样本位置见[开发文档](development.md#d3-a1-视图回归)。
