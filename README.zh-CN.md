[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 3; status: design-review -->

# Pixstride

Pixstride 是一个 Windows 原生图片查看器项目，目标是快速启动、低延迟交互和有界的资源占用。

项目受 MassiGra 轻量化界面和快速交互体验启发，当前处于
Design Review / Repository Foundation（设计评审与仓库基础建设）阶段。仓库目前包含文档和工程管理工具，尚无查看器应用、Cargo 工程、
可下载版本或产品性能实测结果。

## 计划采用的基础技术

已选定 Rust Stable、通过 windows-rs 调用的 Win32、Direct3D 11 和 DXGI Flip Model
交换链（DXGI Swap Chain），目标平台为 Windows 10/11 x64。普通图片优先评估 WIC 解码。

目前推荐由独立渲染线程管理 GPU 资源，通过有界后台任务解码图片，并为超大图片设置独立路径。
线程方案、缓存预算和分块尺寸仍需实验验证；测试计划中列出的格式不代表已经支持。

## 阅读和参与

- [架构范围与评审整改](docs/zh-CN/architecture.md)
- [开发状态与文档检查](docs/zh-CN/development.md)
- [跨格式压力测试计划](docs/zh-CN/stress-testing.md)
- [写作规范](docs/STYLE_GUIDE.zh-CN.md)与[术语表](docs/TERMINOLOGY.zh-CN.md)
- [贡献指南](CONTRIBUTING.zh-CN.md)
- [GitHub 工作流、许可证与发布计划](docs/zh-CN/maintenance.md)

文档工具仅使用 Python 3.11 及以上版本的标准库：

```sh
python -B scripts/check_docs.py
python -B -m unittest discover -s scripts/tests
```

私人样本、设计草稿、机器检测报告和生成的缓存应保存在仓库外。通过本地配置或环境变量提供
测试路径，不要在 Issue 中附加私人图片或未经脱敏的诊断信息。

## 许可证

Pixstride 采用 MIT 或 Apache-2.0 双许可证，使用者可以自行选择其中一种许可证。
完整条款见 [LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)。
SPDX 许可证表达式为 `MIT OR Apache-2.0`。

第三方组件保留各自的许可证和声明要求。
