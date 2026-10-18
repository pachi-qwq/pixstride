[English](../en/development.md) | [简体中文](development.md)

<!-- doc-id: development; revision: 2; status: design-review -->

# 开发状态与检查

仓库目前包含公开文档、模板和 Python 文档工具，没有 Cargo Workspace 或查看器程序。
先运行文档检查；阶段 A 实现获单独批准后，Rust 构建命令才适用。

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

## 后续 Windows 环境准备

阶段 A 需要 Rust Stable、`x86_64-pc-windows-msvc`、Cargo、rustfmt、Clippy、MSVC C++
构建工具和兼容的 Windows SDK。先检查现有版本，再决定安装或更改。
评审后采用项目局部工具链选择，不更改其他项目的默认工具链。

此前的本机探针使用 Rust/Cargo 1.96.0 和 windows-rs `windows` 0.62.2，
经 MSVC 编译后成功创建 WIC Factory 和 D3D11 硬件设备。
这只验证了单机上的特定组合，不代表项目已固定这些版本，也不代表干净机复现或性能成绩。

Debug 设备创建返回 `0x887A002D`，缺少 SDK 调试组件。
Graphics Tools 安装可能需要管理员权限，必须先获批准。
PresentMon 2.6.0 CLI 的帮助输出、官方校验值和签名已在本地核实，实时采集尚未验证。
WPR profile 查询成功，WPA/trace 分析和交换链、Shader、WARP 仍待验证，原始日志保存在内部。

安装和调研参考 [Rust Windows MSVC 指南](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html)、
[windows-rs](https://github.com/microsoft/windows-rs)、
[Microsoft 图形工具说明](https://learn.microsoft.com/en-us/windows/ai/directml/dml-debug-layer)、
[WPA](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/windows-performance-analyzer)
和 [PresentMon](https://github.com/GameTechDev/PresentMon)。

## Cargo 工程获准后

加入 Windows Runner 作业，运行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、
`cargo test --locked` 和 `cargo build --release --locked`。先解析并固定依赖，检查实际启用的 feature。
不要为运行 CI 创建占位查看器。共享 CI 验证正确性和构建，专用硬件测量延迟和资源峰值。
发布改动前阅读[阶段 A 范围](architecture.md)和[维护说明](maintenance.md)。
