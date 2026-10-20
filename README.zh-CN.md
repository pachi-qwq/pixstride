[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 7; status: stage-c-draft-review -->

# Pixstride

Pixstride 是一个 Windows 原生图片查看器项目，目标是快速启动、低延迟交互和有界的资源占用。

项目受 MassiGra 轻量化界面和快速交互体验启发，已完成阶段 A 基础和阶段 B 图片浏览。JPEG、PNG、BMP 打开、导航、
有界缓存和长按进度显示已通过负责人验收。阶段 C 的 PNG 流式处理和分块渲染正在进行
[Draft PR 评审](https://github.com/pachi-qwq/pixstride/pull/4)。负责人已确认真实图片缓存命中运行正常，性能及详细压力/视觉验收仍待完成。
项目尚无公开发行版。

## 当前基础

已选定 Rust Stable、通过 windows-rs 调用的 Win32、Direct3D 11 和 DXGI Flip Model
交换链（DXGI Swap Chain），目标平台为 Windows 10/11 x64。阶段 A 建立窗口与渲染基础；阶段 B 加入 WIC 解码和目录导航。

目前由独立渲染线程管理 GPU 资源，通过有界后台任务解码图片，并为超大图片设置独立路径。
线程方案、缓存预算和分块尺寸仍需实验验证；测试计划中列出的格式不代表已经支持。

## 阅读和参与

- [阶段 A 实现与验证](docs/zh-CN/stage-a.md)
- [阶段 B 图片浏览实现与限制](docs/zh-CN/stage-b.md)
- [阶段 C 超大 PNG 实现与验证](docs/zh-CN/stage-c.md)
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
