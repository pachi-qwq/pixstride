[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 2; status: design-review -->

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

## 文档与贡献

架构摘要、贡献指南和文档检查工具正在独立文档分支上整理，等待评审。
查看器实现需要单独批准的开发任务。

私人图片、内部草稿、机器报告、大型测试数据和生成缓存不进入 Git。
具有合法授权的项目资源和经过评审的小型测试样本可以放在 `assets/` 和 `tests/fixtures/`。

## 许可证

Pixstride 采用 MIT 或 Apache-2.0 双许可证，使用者可以自行选择其中一种许可证。
完整条款见 [LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)。
SPDX 许可证表达式为 `MIT OR Apache-2.0`。

第三方组件保留各自的许可证和声明要求。
