[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 5; status: stage-b-local-review -->

# Pixstride

Pixstride 是一个 Windows 原生图片查看器项目，目标是快速启动、低延迟交互和有界的资源占用。

项目受 MassiGra 轻量化界面和快速交互体验启发，已完成阶段 A 基础并进入阶段 B 评审。本地功能分支加入了
JPEG、PNG、BMP 打开与浏览的首版代码，真实图片图形界面验收仍待完成。项目尚无公开发行版，也没有已验证的阶段 B 性能报告。

## 当前基础

已选定 Rust Stable、通过 windows-rs 调用的 Win32、Direct3D 11 和 DXGI Flip Model
交换链（DXGI Swap Chain），目标平台为 Windows 10/11 x64。阶段 A 建立窗口与渲染基础；阶段 B 加入初版 WIC 解码和目录导航，
仍需完成图形界面验收。

目前推荐由独立渲染线程管理 GPU 资源，通过有界后台任务解码图片，并为超大图片设置独立路径。
线程方案、缓存预算和分块尺寸仍需实验验证；测试计划中列出的格式不代表已经支持。

## 阅读和参与

- [阶段 A 实现与验证](docs/zh-CN/stage-a.md)
- [阶段 B 图片浏览实现与限制](docs/zh-CN/stage-b.md)
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
