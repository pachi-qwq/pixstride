[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 9; status: gif-poster-review -->

# Pixstride

Pixstride 是一个 Windows 原生图片查看器项目，目标是快速启动、低延迟交互和有界的资源占用。

项目受 MassiGra 轻量化界面和快速交互体验启发，阶段 A–C 均已完成并合并。
[阶段 C PR #4](https://github.com/pachi-qwq/pixstride/pull/4) 记录了负责人提供的六项 Windows 人工验收，结果全部通过。
项目尚无公开发行版。阶段 D 已开始技术验证，本分支实现了 GIF 首帧支持，其余范围仍需负责人批准。

## 当前基础

已选定 Rust Stable、通过 windows-rs 调用的 Win32、Direct3D 11 和 DXGI Flip Model
交换链（DXGI Swap Chain），目标平台为 Windows 10/11 x64。已合并的实现包括：

- Win32 原生 UI 和 D3D11 硬件渲染。
- JPEG/PNG/BMP 浏览、WIC 后台解码、同目录导航、CPU/GPU 缓存、相邻预加载及 GPU 缓存命中快速切换。
- 超大 PNG 行流式解码、磁盘 Tile Pyramid、视口分块渲染、缩放和平移。
- Generation 过期任务保护、有界 CPU/GPU 缓存驻留，以及独立的大图资源预算。

本分支还实现了 [D1-a GIF 首帧](docs/zh-CN/stage-d1a.md)：先进行有界结构校验，再将首个图像帧合成到透明画布。
尚未实现 GIF 动画播放，本次变更的桌面目视验收仍待完成。

目前由独立渲染线程管理 GPU 资源，通过有界后台任务解码图片，并为超大图片设置独立路径。
线程方案、缓存预算和分块尺寸仍需实验验证；测试计划中列出的格式不代表已经支持。

超大 PNG 首次完整缓存构建较慢，尚无渐进式预览；历史视口长尾尚未完全归因。Device Lost、实际磁盘耗尽、分配失败和
长期稳定性仍需更全面的测试。部分 PNG 编码及 ICC/HDR 尚未支持，具体见[阶段 C 限制](docs/zh-CN/stage-c.md)。
Present 返回不代表画面已经显示到屏幕。

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
