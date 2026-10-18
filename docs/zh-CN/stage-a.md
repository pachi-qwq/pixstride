[English](../en/stage-a.md) | [简体中文](stage-a.md)

<!-- doc-id: stage-a; revision: 7; status: implementation-review -->

# 阶段 A：Win32 窗口与 D3D11 渲染

阶段 A 是 Pixstride 的第一个可运行切片，用来检查 Win32 原生窗口、独立 D3D11 渲染线程和按需 DXGI
呈现路径是否适合项目。它不读取图片文件，也不实现图片导航。

## 线程职责与事件流

UI 线程创建并维护 `HWND`，处理 Win32 消息循环，更新最新渲染状态，并触发 Windows auto-reset event。
渲染线程创建并维护 D3D11 Device、Immediate Context、Swap Chain、Shader、Render Target、Sampler
和两张测试纹理。D3D11 COM 接口不会跨线程传递。渲染线程释放资源并发送 `WM_RENDER_STOPPED` 前，
UI 线程会保持 `HWND` 有效。

```mermaid
flowchart LR
  Msg[Win32 消息] --> UI[UI 线程]
  UI --> State[Mutex：最新快照]
  UI --> Event[Auto-reset 工作事件]
  Event --> Render[渲染线程]
  Latency[DXGI 帧延迟句柄] --> Render
  Render --> D3D[D3D11 Immediate Context]
  D3D --> Swap[Flip Discard Swap Chain]
  Render --> Done[WM_RENDER_STOPPED]
  Done --> UI
```

`Arc<Shared>` 通过 `Mutex<PendingState>` 在线程间共享少量状态，里面包括当前测试纹理、客户区尺寸、
序号、重绘标志和关闭标志。新状态会覆盖旧快照，不会逐个积累渲染任务，因此快速翻动输入不会形成无界队列。
auto-reset event 只负责唤醒渲染线程，不存储渲染状态。有待绘制帧时，渲染线程等待工作事件或 DXGI 帧延迟句柄；
没有待绘制帧时，只等待工作事件，不轮询。

窗口过程从 `GWLP_USERDATA` 读取稳定的 `Box<WindowContext>` 指针。UI 线程在 `WM_NCCREATE` 安装指针，
在 `WM_NCDESTROY` 清除。收到关闭请求后，UI 线程通知渲染线程并继续处理消息。渲染线程先清空
Immediate Context，释放 Back Buffer View 等 COM 包装器，再发送 `WM_RENDER_STOPPED`；UI 收到后才销毁窗口。
`ResizeBuffers` 也只在渲染线程执行，并且会先解除 Context 对旧 Back Buffer 的绑定、释放旧 Render Target View。
最小化时客户区尺寸为零，渲染暂停。

## D3D11 与呈现路径

程序先请求 Feature Level 11.1 或 11.0 的硬件 D3D11 Device。Device 创建失败时尝试 WARP；启动日志会记录
实际 Driver Type、Feature Level 和 DXGI Adapter 名称。后续 Swap Chain 或 Shader 创建失败时不会再次回退。
当前实现创建两个缓冲区的 `DXGI_SWAP_EFFECT_FLIP_DISCARD` Swap Chain，
启用 `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`，将最大帧延迟设为 1，绘制前等待帧延迟句柄，
并调用 `Present(1, 0)`。

程序启动时在内存中生成两张 256 × 256 RGBA8 测试图：棋盘格和渐变图。每张图各创建一个 D3D11 Texture2D
和 Shader Resource View。按 Left 或 Right 只切换已有视图，不会重新生成或上传像素。全屏三角形通过线性
Sampler 采样所选纹理，窗口会把纹理拉伸至客户区大小。

`build.rs` 使用 Windows SDK 中的 `fxc.exe` 编译 `shaders/test_pattern.hlsl`，将 DXBC 字节码写入 Cargo
的 `OUT_DIR`，并嵌入可执行文件。程序正常启动或绘制帧时不会重复编译 Shader。如果自动发现 Windows SDK
目录失败，可将 `PIXSTRIDE_FXC` 设置为 `fxc.exe` 的完整路径。

如果 `Present` 失败，渲染器会查询 `GetDeviceRemovedReason`，报告错误并退出。阶段 A 尚未实现 Device 重建和恢复。
Rust COM 包装器会在渲染器于所属线程析构时释放引用。UI 线程不持有 D3D11 资源。

## 计时范围

程序使用 `std::time::Instant`。它在 Windows 上单调递增，并由系统高精度时钟提供支持。每项指标最多保留最近
4096 个样本，形成固定容量环形缓冲区；退出时输出样本数、最小值、最大值、P50、P95 和 P99，分位数使用最近秩法。
程序不会在每帧同步写日志。

| 指标 | 测量范围 |
|---|---|
| 启动 | 进程入口到 `CreateWindowExW` 返回；不代表窗口已可见或已合成显示。 |
| D3D11 初始化 | Device、Swap Chain、已编译 Shader、测试纹理上传和初始 Render Target 创建。 |
| Device 创建 | `D3D11CreateDevice` 耗时；若硬件创建失败，也包括 WARP 重试。 |
| Swap Chain 创建 | 创建 DXGI Swap Chain 并设置帧延迟的耗时。 |
| Shader 初始化 | 创建 Vertex Shader、Pixel Shader、Sampler 和 Rasterizer State 的耗时。 |
| 测试纹理准备 | CPU 生成测试像素、创建 D3D11 Texture2D 和 Shader Resource View 的耗时。该项测 CPU/API 路径，不等待 GPU 完成。 |
| 首次 Present | 从 Render Thread 启动到首个 `Present` 调用成功返回的耗时，包括初始化和首帧工作。 |
| 输入处理 | 方向键处理到发布最新状态并触发事件。 |
| 请求到渲染线程 | 发布快照的时间到渲染线程读取该快照。 |
| 渲染命令 | CPU 设置 D3D11 状态、清理 Render Target 并提交三角形绘制的时间。 |
| Present | `Present(1, 0)` 调用耗时，不代表画面显示耗时。 |
| Resize | 渲染线程调整缓冲区并重建 Render Target 的 CPU 耗时；零尺寸暂停不计入。 |
| 关闭 | UI 发出关闭请求到渲染线程退出；不包括所有 OS 进程清理。 |

这些计时无法证明画面何时到达显示器。可用 PresentMon/ETW 观察已显示帧事件；Input-to-Photon 还需外部高速
拍摄方法。当前程序没有集成这些工具。首次 Present 是应用内阶段记录，不是屏显时间；不能把 `Present` 快速返回解释成屏显延迟结果。

两次本机 Release 启动记录的进程入口到 `CreateWindowExW` 返回时间分别为 2.824 ms 和 4.762 ms，
D3D11 初始化时间分别为 223.234 ms 和 206.379 ms。这只是同一台机器上的两次诊断样本，未控制冷/热启动条件，
不足以形成可靠的分位数报告。两次日志都记录首个 `Present` 调用成功返回。这些运行早于 Driver Type 和 Adapter 名称日志，
因此无法确认使用硬件驱动还是 WARP。新的分项指标尚未通过正常关闭流程采集到数值。

## 构建和运行

需要 Windows 10/11 x64、Rust Stable、`x86_64-pc-windows-msvc` Target、MSVC C++ Build Tools、
Windows SDK 和 `fxc.exe`。

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
cargo run --release
```

Release 可执行文件位于 `target/release/pixstride.exe`。关闭程序后，stdout 会输出计时汇总。Rust CI 在
`windows-latest` 上运行格式检查、Clippy、测试、Release 构建和 Rustdoc；它不运行交互式 GUI 测试，也没有修改
GitHub Ruleset 的必需检查。

## 验证状态

本地 Rust 验证已通过：格式检查、禁止警告的 Clippy、7 项单元测试、Release 构建和 Rustdoc 生成。
单元测试覆盖有界计时样本、分位数计算、测试纹理数据、最新状态合并和终止关闭状态。

本机启动测试创建了 Win32 窗口，`IsWindowVisible` 返回可见，并输出 D3D11 Device 创建成功，Feature Level 为 11.1（`0xB100`）。
当前自动化环境没有暴露原生窗口供视觉检查，所以纹理是否实际显示、方向键切换、交互式 Resize/最小化/恢复、正常关闭、
连续启动关闭和输入延迟分布都尚未验证。Draft PR 的 docs 和 Rust CI 均已通过。Windows Rust Job 完成格式、Clippy、测试、
Release 构建和 Rustdoc 检查；CI 不会验证交互式 GUI 行为。

因此，独立渲染线程目前只是待评审的实现，尚未通过延迟实测。阶段 A 仍需在可交互的 Windows 桌面采集唤醒到渲染
和 `Present` 样本，并与 50 ms 启动、5 ms 缓存切换处理和 1 ms 输入处理的工程目标比较。这些数值仍是目标，不是成绩。
