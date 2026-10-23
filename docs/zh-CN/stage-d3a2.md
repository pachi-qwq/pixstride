[English](../en/stage-d3a2.md) | [简体中文](stage-d3a2.md)

<!-- doc-id: borderless-fullscreen; revision: 2; status: fullscreen-review -->

# D3-a2：无边框全屏与窗口状态恢复

本地分支在已合并的 D3-a1 几何基础上增加 F11 无边框全屏和 Esc 恢复。
自动化验证通过，负责人 Windows 人工验收及 PR 审核待完成。
继续复用既有 HWND、Device、Immediate Context、Swap Chain 和 Render Thread，不改变显示模式或刷新率，
不调用 `SetFullscreenState(TRUE)`。

## 窗口状态与所有权

UI 线程在既有 `WindowContext` 内维护 `window::fullscreen::State`。普通/最大化使用原生窗口状态；
进入前只保存一次 GWL_STYLE、GWL_EXSTYLE、WINDOWPLACEMENT 及原显示器矩形/设备名。
全屏、最小化/恢复和显示器变化期间保留这份快照，成功恢复后才清除。
快照仅用于当前窗口的本次恢复，不增加磁盘配置持久化。

Entering 和 Exiting 标记同步事务。只有原生操作和实际全屏矩形确认成功后才提交 Fullscreen。
进入失败尝试恢复一次，退出失败尝试返回无边框一次；两者都失败时，Recovery 保留原快照，
F11/Esc 可再次尝试恢复。错误写入 stderr，不弹出模态框，不循环重试或创建替代 HWND。

`SetWindowPos` 和 `SetWindowPlacement` 可能同步重入窗口过程。
事务处理放在普通窗口过程建立上下文独占借用之前；每次原始指针字段访问在调用窗口 API 前结束，
过渡状态阻止嵌套全屏事务。原有 WM_NCDESTROY、Renderer/Worker 完成通知与析构顺序仍然生效。

## 进入、退出与输入

进入先保存原生状态，再用 `MonitorFromWindow` 按最大相交面积选择显示器，完全屏外时取最近显示器，
通过 `GetMonitorInfoW` 获取范围。移除普通窗口框架/最大化样式及相关扩展边框位，
以 SWP_FRAMECHANGED 调用 `SetWindowPos` 覆盖 **rcMonitor**，包括任务栏区域。
rcWork 仅用于普通窗口恢复。边框刷新遵循
[SetWindowPos 契约](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)。
保留激活、Owner 和 Z 序，不增加永久 Topmost，也不修改 Explorer 设置。

退出恢复原样式和曾修改的扩展边框位，用保存的 WINDOWPLACEMENT 调用 `SetWindowPlacement`，
再刷新非客户区，不额外移动或调整尺寸。普通位置、尺寸及最大化状态以原生 Placement 为准，不另建一套位置模型。
Placement 的工作区坐标只交回对应 API，绝不作为屏幕坐标传给 `SetWindowPos`。
依据见 [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement)
及 [SetWindowPlacement](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowplacement)。

F11 切换全屏，自动重复消息只被消费，不再次切换。Esc 仅退出全屏/恢复待重试状态，不新增普通窗口退出命令。
方向键释放、F/1、Ctrl+O 和 Space 保留已有语义。Alt+F4 继续使用 `DefWindowProc` 及异步 WM_CLOSE 路径。
开始全屏事务时终止当前拖动。

## 显示器、DPI 与视口连续性

屏幕坐标允许负数。全屏采用当前显示器物理屏幕矩形；WM_SIZE 用 `GetClientRect` 查询真实客户区，
避免 16 位打包尺寸截断。最小化发布零尺寸，相同尺寸不重复发布渲染工作。
保留 Per-Monitor V2 及 100% 下一个图像像素对应一个客户区物理像素的定义。

全屏 WM_DPICHANGED 使用当前显示器范围，不采用普通带边框窗口的建议矩形。
WM_DISPLAYCHANGE、WM_MOVE 和恢复尺寸通知让全屏重新对齐当前最近显示器；矩形未变时不再定位，最小化时不擅自恢复。
退出先由原生 Placement 处理过期屏外位置。原矩形处无法匹配保存的显示器身份时，普通窗口另行放入当前最近工作区，
仅在放不下时缩小。负坐标及大跨度使用经过校验的尺寸和 i64 中间值。
真实显示器移除和混合 DPI 设备组合尚未验证。

Resize 继续使用 D3-a1：保留额外 Zoom，将旧客户区中心下的原图位置放到新中心。
Fit 重算基础倍率，100% 保持物理像素。全屏不是切图，不推进 Generation、不重置视图、不清空图片缓存，
也不重启 GIF Session、重置 Loop 或改变 Delay 调度。真实最小化及文件对话框仍沿用 D1-b2 暂停规则。
Tile LOD/缓存格式及全部额度保持原样：普通单图 256 MiB，CPU/GPU 共享缓存各 512 MiB，原有子额度继续生效。

## 验证与测量

样本通过 `PIXSTRIDE_TEST_ROOT` 在 Git 外生成，未操作私人极限 PNG 或其既有缓存。完整 Rust 测试通过 108 项，
40 项显式专项默认忽略。选定的 23 项 Windows Release 诊断逐进程运行全部通过，
包括六项新增全屏探针/样本准备及 D3-a1 的 17 项回归。

| 检查 | 结果 |
|---|---|
| 原生事务 | 普通/调整尺寸/最大化恢复，F11 重复和 Esc，相同范围通知去重，实际全屏客户区，样式/Placement/位置/边框失败注入、回滚及重试。 |
| 显示器回退 | 负坐标、溢出/零尺寸、模拟原显示器身份消失及过期位置、实际移动/DPI 通知；不代表真实热插拔或 DPI 迁移验收。 |
| GPU 输出 | 八种实际窗口/全屏 Swap Chain 场景，共 33,644,544 像素，对独立双线性/Alpha 参考，通道容差 2；Device、Swap Chain、SRV 身份不变，静态样本载荷 16 字节。 |
| Viewer | 12 次静态、八次播放中 GIF 切换，观察中心保持，无静态重复解码/上传，Session/Generation 稳定，暂停/最小化/对话框标记、有限 Loop 稳定、GIF/普通图/Tile 导航和全屏播放中关闭。 |
| 导航 | 独立全屏普通图/GIF 慢 Worker 长按、反向及 KeyUp 诊断，最终目标和提交 Generation 均为 61。 |
| 空闲/资源 | 门控静态/暂停/结束观察期不新增 Present 或渲染唤醒；预热后 40 次原生往返的进程/GDI/USER 句柄计数不变。 |

原生计时探针测量同步按键分发到 UI 事务完成，包含期间产生的原生消息，不启动 Renderer。
另一个 Viewer 探针测量实际 Swap Chain Resize 和 GIF Deadline。以下为单机两轮验证中每轮分位数/最大值的范围，
不是合并样本的分位数或基线对照。
环境为 Windows、RTX 4080 SUPER、Feature Level 11.1、Rust 1.96.0、Release/locked，
3840×2160 显示器、DPI 168（175%）。合成 DPI-144 消息不能证明真实 150% 场景通过。

| 指标 | 每轮样本数 | P50 / P95 / 最大值范围（ms） |
|---|---|---|
| UI 进入事务 | 40 | 0.494–0.724 / 3.449–13.153 / 4.240–18.816 |
| UI 退出事务 | 40 | 0.881–1.276 / 3.686–14.832 / 3.840–19.970 |
| Viewer Swap Chain Resize | 36 | 1.086–1.103 / 1.690–1.954 / 1.769–2.280 |
| Viewer GIF Deadline Lateness | 25 | 5.137–5.373 / 12.773–14.379 / 15.039–15.424 |
| 独立普通图暖 GPU 命中 → Present 返回 | 110 | 0.235–0.253 / 6.594–6.731 / 7.095–7.164 |

原生探针预热后往返，进程/GDI/USER 计数均保持 144/2/4。
Viewer 在全屏 GIF 播放中关闭，完成清理耗时 19.388/22.844 ms（每轮一个样本）。
动画字节、共享 CPU 字节、Credit 和排队输出最终均为零。小样本报告普通 CPU/GPU 峰值为 672/720 字节，
动画峰值 192/48 字节，Credit 峰值 2；Tile CPU/GPU 峰值为 1,374,840/1,387,380 字节。
独立 Tile 回归达到 63,197,532/93,206,000 字节，仍在既有限制内。
这些是报告的载荷类别，不能相加后作为进程/VRAM 总量；计数器不隔离 Swap Chain、WIC、驱动及 OS 分配。
后一次验证仅在测试断言补充后运行，生产代码和输入序列相同。未隔离桌面调度，原生事务 P95 的波动未作归因。
小负载和两轮计时不能证明长期稳定、延迟等价或性能提升。

所有计时均结束于应用侧事件，Present 返回不代表实际显示完成或 Input-to-Photon。
本轮未归因或优化 [Issue #11](https://github.com/pachi-qwq/pixstride/issues/11)，
保留原有静态 GPU 命中 P95 增加 0.394–0.905 ms 的证据及 DXGI 帧准入修复。

## Windows 人工验收：待完成

以下清单须由项目负责人实际操作。自动化自有窗口测试不能证明目视闪烁、任务栏/Alt+Tab 行为或所有物理显示器组合。

1. F11 正常进入/退出；长按 F11 不反复振荡。
2. Esc 正常退出全屏，普通窗口中不关闭程序。
3. 调整过尺寸的普通窗口恢复原位置和尺寸。
4. 最大化窗口恢复后仍最大化，再还原时保留此前普通位置。
5. Fit/100%、Zoom/Pan 在进入/退出期间保持当前观察区域。
6. GIF 播放/暂停、有限/无限 Loop 状态正确。
7. 全屏下长按、反向、Ctrl+O/取消正常。
8. Alt+Tab、Alt+F4、最小化/恢复正常。
9. 连续快速 F11 无持续黑屏、卡死或不可接受的闪烁。
10. 普通图及生成的大 PNG 四角、中心、Tile 接缝正常。
11. 有设备时检查 100%/150%/200%、混合 DPI、不同分辨率、负坐标及显示器移除。
12. 全屏关闭后无孤儿进程或明显资源增长。

所有项目均待负责人确认。WIC 不可强制抢占/内部分配、完整 Device Lost 恢复、真实资源耗尽和长期稳定性限制保留。
Stage C 首次构建慢/无渐进预览及历史视口长尾仍是独立事项。
复测命令和保留的生成样本见[开发文档](development.md#d3-a2-全屏回归)。
