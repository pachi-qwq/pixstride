[English](../en/development.md) | [简体中文](development.md)

<!-- doc-id: development; revision: 17; status: view-transform-review -->

# 开发状态与检查

仓库包含一个 Win32/D3D11 查看器 Cargo Package。阶段 B 加入 JPEG、PNG、BMP 的 WIC 解码、目录导航和有界
CPU/GPU 缓存。负责人已确认真实图片显示、缩放、键盘导航、原生文件对话框和窗口 Resize 正常。阶段 A–C 均已合并，
阶段 C 提供独立预算的 PNG 流式处理、磁盘 Tile Pyramid 构建及支持缩放/平移的视口渲染，六项 Windows 人工验收全部通过。
缓存变量、构建测量、支持的编码和人工检查见[阶段 C](stage-c.md)。项目尚无公开发行版；阶段 D 已合并 EXIF 1–8
修正、GIF 静态首帧、后台 Worker 和 [D1-b2 GIF 播放](stage-d1b2.md)，负责人报告其八项 Windows 人工验收全部通过。
本分支增加 [D3-a1 统一视口几何](stage-d3a1.md)，负责人验收和 PR 审核仍待完成。
后续阶段 D 范围仍需批准。

## 构建和运行

需要 Windows 10/11 x64、Rust Stable 和 `x86_64-pc-windows-msvc` Target、MSVC C++ Build Tools、
Windows SDK 及 `fxc.exe`。构建脚本会将 HLSL 编译为 DXBC 并写入 Cargo 的 `OUT_DIR`；如果 SDK 自动发现失败，
将 `PIXSTRIDE_FXC` 设置为 `fxc.exe` 完整路径。程序正常运行时不会启动 Shader 编译器。

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
cargo doc --no-deps --locked
cargo run --release
```

不带参数时，程序显示阶段 A 生成的测试纹理。传入一个图片路径可打开 JPEG、PNG、BMP 或 GIF（支持多帧播放，Space 暂停/恢复）；按 `Ctrl+O` 打开原生文件对话框，
按 Left/Right 浏览当前目录，按 `F` 选择 Fit，按 `1` 选择 1:1 物理像素。本分支普通图、GIF 及符合条件的超大 PNG Tile
共用鼠标锚点缩放和左键拖动平移。切图保留模式、重置额外 Zoom/Pan；GIF 换帧保留视图。Resize 保持客户区中心下的图像位置。退出时，stdout 输出有界计时样本和缓存计数。
Release 可执行文件为 `target/release/pixstride.exe`。功能范围和限制见 [阶段 B](stage-b.md) 与 [阶段 C](stage-c.md)。

## D3-a1 视图回归

从仓库根目录运行，使用 Git 外独立目录存放生成样本和 Tile 数据。以下探针只使用合成输入，不打开私人压力图片：

```powershell
$env:PIXSTRIDE_TEST_ROOT = [IO.Path]::GetFullPath('..\..\logs\pixstride-d3a1\samples')
$env:PIXSTRIDE_TILE_CACHE = [IO.Path]::GetFullPath('..\..\logs\pixstride-d3a1\tile-cache')
cargo test --release --locked release_view_transform_gpu_pixels -- --ignored --nocapture
cargo test --release --locked release_view_transform_input_and_playback -- --ignored --nocapture
cargo test --release --locked release_frame_admission_survives_stale_selection -- --ignored --nocapture
cargo test --release --locked release_animation_prepare -- --ignored --nocapture
cargo test --release --locked release_prepare_gif_poster_samples -- --ignored --nocapture
cargo test --release --locked release_prepare_gui_samples -- --ignored --nocapture
```

几何探针输出 `pixels_checked=7869`，输入探针核对 120 次变换未新增解码/上传。生成器会输出保留样本的目录。
人工检查时打开 `animation-fixtures/partial.gif`、Poster 目录的 JPEG/PNG/BMP，以及大图样本输出目录内的 `wide.png`，
用 Ctrl+O 在这些目录间切换。保留独立 Tile 缓存，不要将探针指向既有私人缓存。
验收标准见 [D3-a1](stage-d3a1.md#待验收与限制)。

## 当前可执行的检查

需要 Git 和 Python 3.11 及以上版本，无需安装 Python 包。在仓库根目录运行：

```sh
python -B scripts/check_docs.py
python -B -m unittest discover -s scripts/tests
```

检查器读取已跟踪文件和未被忽略的候选文件，也检查后来被忽略的已跟踪文件。
硬检查覆盖 Markdown 结构、相对链接/锚点、双语入口/修订号/共享字面值、文件大小和已知私人路径/凭据模式。
诊断不会回显匹配到的秘密内容。
图片经来源、隐私和许可人工评审后，可放在 `assets/` 和 `tests/fixtures/`，每个文件不超过 1 MiB。
检查器只约束其文件大小，不解码或扫描二进制元数据；其他位置的图片、私人数据、大型生成文件和缓存
不属于公开输入。

文字检查会识别围栏和缩进代码、行内代码、链接目标、API 标识符、引用的改写示例和 HTML 注释。
中文空格、标点、可疑术语拼写和部分套话只作提示，应按句子判断，不机械替换。
隐私检查仍扫描代码和注释。工具仅实现明确支持的 GFM 子集，不是完整解析器或秘密扫描器，
特殊语法仍需人工核对。

按需运行网络链接检查：

```sh
python -B scripts/check_docs.py --external-links
```

网络检查只验证 HTTP(S) 可达性，不验证外部锚点或技术结论。
明确的 404/410 会使检查失败；认证、限流、超时和请求被阻断时需人工核对。
常规 CI 运行确定性的本地检查，网络步骤按需手动执行。

测试只创建临时文件。默认临时目录不合适时，通过 `PIXSTRIDE_DOC_TEST_ROOT` 指定已存在的仓库外目录。
报告也写在仓库外。`-B` 防止 Python 在工作区生成字节码。

## 已验证的本地工具链与限制

当前验证环境有 Rust/Cargo 1.96.0、`x86_64-pc-windows-msvc` Target、Visual Studio Build Tools 2022
（MSVC 14.44）和 Windows SDK 10.0.26100.0。项目依赖 `windows` 0.62.2 和 `anyhow` 1.0.104；
`Cargo.lock` 记录了完整解析结果。阶段 A 的 Release 构建通过 FXC 编译两个 HLSL 入口，阶段 C 另加 Tile Vertex Shader 入口。

项目负责人在 NVIDIA GeForce RTX 4080 SUPER、Hardware Driver、Feature Level 11.1 的机器上运行 Release 程序，
人工确认棋盘格、Left/Right 纹理切换、最大化/最小化/恢复、连续 Resize 和 Alt+F4 关闭正常。测得 D3D11 初始化
183.236 ms，Device 创建 177.742 ms。完整计时及其限制见[阶段 A 验证](stage-a.md)。受控冷/热启动、Input-to-Photon、
Device Lost 压力测试和长期稳定性仍未验证。仅通过编译不能证明 GUI 行为正常。

早期环境探针的 Debug Device 创建返回 `0x887A002D`，原因是缺少 SDK 调试组件。当前程序未启用 D3D11 Debug Layer。
安装 Graphics Tools 可能需要管理员权限，须另行批准。此前已核实 PresentMon 2.6.0 CLI 帮助、官方校验值和签名；
实时采集与 WPA 跟踪分析仍未验证。探针日志保存在仓库外。

安装和调研参考 [Rust Windows MSVC 指南](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html)、
[windows-rs](https://github.com/microsoft/windows-rs)、
[Microsoft 图形工具说明](https://learn.microsoft.com/en-us/windows/ai/directml/dml-debug-layer)、
[WPA](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/windows-performance-analyzer)
和 [PresentMon](https://github.com/GameTechDev/PresentMon)。

## CI 与性能验证

`Rust` 工作流使用 Windows Runner，Job 名为 `rust`。它通过锁文件运行格式检查、Clippy、测试、Release 构建和 Rustdoc。
工作流权限为只读，不发布 Release。本轮没有修改必需状态检查或仓库保护设置。

程序使用 `std::time::Instant` 计时，每类最多保留最近 4096 个样本，分位数采用最近秩法。启动范围结束于
`CreateWindowExW` 返回，不代表首帧已显示；`Present` 耗时也不代表屏显延迟。可用 PresentMon/ETW 观察已显示帧，
用外部方法测量 Input-to-Photon。当前实现与限制见[阶段 A 验证](stage-a.md)、[阶段 B](stage-b.md)、[阶段 C](stage-c.md)、[架构](architecture.md)
和[维护说明](maintenance.md)。
