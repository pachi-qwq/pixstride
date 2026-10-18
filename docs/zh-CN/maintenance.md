[English](../en/maintenance.md) | [简体中文](maintenance.md)

<!-- doc-id: maintenance; revision: 2; status: proposal -->

# 仓库维护与发布

本页规定计划采用的工作流和需要负责人批准的事项。
本地配置文件不会自动启用 GitHub 设置，也不会发布内容。

## 空仓库初始化

2026-10-18 的只读审计发现远程为公开仓库，尚无分支和提交，提交 API 返回 HTTP 409。
仓库元数据中的默认分支名为 `master`，rulesets 列表为空。
发布前必须重新检查远程，不能根据曾经为空的结果覆盖后续内容。

负责人已选择 `MIT OR Apache-2.0` 和 `main`。本地 `main` 保留原始根提交 `95f0d4f`，
其中只有 `.gitignore`、`.editorconfig` 和 `.gitattributes`，再添加双语 README、两份许可证
和范围更明确的忽略规则。通过独立 Worktree 保护已有未跟踪文档。
`docs/repository-foundation` 快进到这一基线，再用独立提交加入评审后的文档和工具。
原始草稿和私人样本均未进入 Git 历史。

新提交通过仓库局部配置使用该账号的 GitHub noreply 身份。根提交保留原有作者元数据，
没有更改全局 Git 设置或既有历史。发布前同时核对文件树和提交元数据。

本地检查和提交完成后，远程发布仍需明确授权。执行前重新检查 refs，远程已变化时停止。
先推送 `main`，核实 README 和许可证，再检查 GitHub 是否已经选它作为默认分支。
仅在需要时修改默认值；操作前核实是否存在真实的 `master` 分支。
随后推送文档分支并向 `main` 建立 PR，不自动合并。首个 PR 包含文档工作流，
需确认 GitHub 实际执行成功后，再把观察到的检查名设为必需。
如果工作流未被调度，报告初始化限制，解决前不强制该检查。后续使用普通 PR 流程。
本计划本身不授权推送、设置修改、历史改写或分支删除。

## 分支和 Pull Request

| 分支 | 用途 |
|---|---|
| `main` | 已评审且检查通过的基线；Cargo 工程存在后保持应用可构建。 |
| `feat/*` | 新功能。 |
| `fix/*` | 缺陷修复。 |
| `docs/*` | 文档。 |
| `refactor/*` | 不以改变功能为目标的重构。 |
| `test/*` | 测试和性能基准。 |
| `chore/*` | 构建及仓库维护。 |

从最新 `main` 创建短期分支，运行相应检查，建立 PR，评审行为、正确性和性能影响，
Squash Merge 后删除已合并功能分支。不建立长期 `develop`。
推荐 Conventional Commits，不增加额外钩子。要求经过 PR，但 required approvals 为 0，
让个人维护者能够自审并合并自己的改动。

## 待批准的远程设置

| 设置 | 建议 | 执行时机 |
|---|---|---|
| 默认分支 | 采用 `main`，仅在 GitHub 尚未自动选择时修改。 | 首次 `main` 推送获准并完成，且重新检查分支后。 |
| PR | `main` 要求经过 PR，required approvals 为 0，不强制其他人或 code owner 批准。 | 初始化后。 |
| Status checks | 要求文档作业 `docs` 成功，第一次运行后核实实际 context。 | CI 已报告检查后；不设置会让必需检查一直 pending 的路径过滤。 |
| 历史 | 禁止强推和删除 `main`，启用 squash 后可考虑线性历史。 | 初始化后，先评审 bypass 策略。 |
| 合并方式 | 优先 squash；初始远程允许 squash、merge 和 rebase。 | 由负责人决定是否关闭其他方式。 |
| 功能分支 | 启用合并后自动删除，初始值为 false。 | 负责人批准后；删除分支仍属远程操作。 |
| 可见性 | 保持现有公开状态。 | 不建议修改。 |

Ruleset 可以要求 PR 而不要求批准人数，也可以禁止强推、删除和要求状态检查。
执行时需核对账户计划支持范围和 bypass 设置，依据为
[GitHub rules 文档](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)。
不要在检查尚不存在时设为必需，也不要设置个人维护者无法满足的批准规则。
Agent 在执行范围内的每项远程操作都必须有明确授权。
自动删除不保证能移除贡献者 fork 中的分支，还取决于 fork 的设置和合并者的写入权限，
详见 [GitHub 分支删除规则](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-the-automatic-deletion-of-branches)。

## 文档 CI

本地工作流名为 `Documentation`，Windows 作业名为 `docs`，只使用 Python 标准库、
只读仓库权限和按 commit 固定的官方 checkout/setup action，不使用秘密。
Push 和 PR 事件不按路径过滤，运行本地检查与检查器测试。
手动 dispatch 可增加外部链接检查，本轮没有推送或在远程执行任何工作流。

硬检查覆盖 Markdown 结构/相对链接、语言入口/修订/共享字面值、已知私人路径/凭据模式，
以及过大或禁止公开的产物。语言质量只作提示，检查器测试覆盖代码、API 名称和链接的误报场景。
工具不是完整 GFM 解析器、秘密扫描服务或翻译校验器。

未来 Cargo 工程存在后，在 Windows 作业中加入 fmt、Clippy、测试和 Release 构建。
硬件延迟/VRAM 测试单独执行。Actions 更新必须可评审，文档工作流不需要自动合并或发布权限。

## 发布前检查

原始 10 份 Markdown 已在仓库外备份和归档，并验证 SHA256。
早期详细设计和机器日志仍属内部资料。公开架构页只提供范围明确的评审摘要，
原稿中的异常路径和设计历史在内部完整保留。

任何推送前都要检查已跟踪文件和暂存 diff，不能只看忽略文件。
存在提交时还要检查可达历史。`.gitignore` 不移除已跟踪文件或旧 blob。
发现私人文件被跟踪时，生成不回显秘密的本地报告，标出提交和路径，暂停发布；
通过负责人处理已暴露凭据的轮换，并提交具体整改方案供批准。
不自动强推、改写历史或删除远程内容。

公开文件不包含本地路径和机器身份。计划中的样本/输出配置见
[`.env.example`](../../.env.example)，真实 `.env` 保持忽略。
大型测试数据、缓存和报告不进入 Git，小型可分发样本需单独评审。
项目图片放在 `assets/`，经过评审的小型测试图片放在 `tests/fixtures/`，这两个目录不按图片扩展名忽略。
文档检查器允许其中的图片不超过 1 MiB，不解码或检查内嵌元数据；来源、隐私和许可证仍须人工评审。
私人数据和生成数据目录继续忽略，已跟踪文件仍会接受检查。

## 许可证决策

Pixstride 采用 MIT 或 Apache-2.0 双许可证，使用者可以自行选择其中一种许可证。
负责人已批准 SPDX 表达式 `MIT OR Apache-2.0`。保留 [LICENSE-MIT](../../LICENSE-MIT)，
版权行为 `Copyright (c) 2026 pachi-qwq`，并保留完整的 [LICENSE-APACHE](../../LICENSE-APACHE)。
MIT 正文已与[官方文本](https://opensource.org/license/mit)核对，
Apache [官方完整文本](https://www.apache.org/licenses/LICENSE-2.0)保持原样。
未来 Cargo Package 使用 `license = "MIT OR Apache-2.0"`，无需创建占位 Cargo 工程。

引入依赖或复用代码前检查授权条件，原样保留必要的第三方版权和许可证声明，标明外部来源。
不将外部代码当作 Pixstride 原创代码。项目许可证不替代第三方义务，也不批准未来任意分发方式。
写作参考资料保留各自的许可证。

潜在 codec 的影响需按精确版本和构建选项审核：

- WIC 使用已安装的 Windows codec，核实扩展可用性和再分发条款，不直接打包系统 DLL。
- libjpeg-turbo 涉及 IJG/BSD 和组件声明；libpng/zlib、libwebp 各有告知要求。
  libavif/libjxl 和所选后端也需核对许可及 IP 声明。
- libheif 采用 LGPL，应明确静态/动态分发和各后端的处理方案。
  LibRaw 提供 LGPL-2.1/CDDL-1.0 选择，附加包或依赖可能改变义务。
- 构建工具、打包样本、字体和复制的文档也要保留来源，生成 SBOM 和完整声明。
  不能只看 Rust crate 的顶层许可。

依据：[libjpeg-turbo 许可](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/main/LICENSE.md)、
[libheif COPYING](https://github.com/strukturag/libheif/blob/master/COPYING)、
[LibRaw 许可](https://www.libraw.org/about)、[libjxl](https://github.com/libjxl/libjxl)、
[libavif](https://github.com/AOMediaCodec/libavif)。
以上是工程审核事项，不表示未来任意链接和分发方式都已获准。

## 版本与 Release

应用存在后，早期版本使用 `0.x.y`，变更记录区分功能、修复、已知限制和已测回归。
缓存与配置 schema 单独版本化。只为已评审构建创建带注释的 `vX.Y.Z` tag。
先发布 x64 便携 ZIP，包含程序、选定运行组件、已定许可证、第三方声明和校验值。
在干净 Windows 验证 CRT 依赖后，才能宣称独立可执行。

后续自动化计划在 Windows 构建/测试，按需要保留符号，生成校验值/SBOM/声明，
上传草稿 Release 供负责人检查。安装、签名、文件关联、更新策略和正式发布需要另行决定。
本轮不实现应用发布流水线。

仍待负责人批准：首次 `main` 推送和默认分支切换、保护/bypass、必需检查、
合并选项、分支删除，以及后续 Release/签名策略。本地评审可以在这些远程操作之前完成。
