[English](AGENTS.md) | [简体中文](AGENTS.zh-CN.md)

<!-- doc-id: agents; revision: 5; status: active -->

# Agent 工作要求

## 当前范围

Pixstride 已进入开发阶段。每项任务只实施负责人批准的范围；阶段 B 已合并，当前阶段 C 覆盖静态超大 PNG 流式处理和分块渲染，
未经批准不添加后续阶段功能。每次任务开始前检查 Git 状态、当前分支、历史和上游。切换分支或整理文件前保护
未跟踪成果，不直接在 `main` 开发功能。

## Rust 源码注释

Rust 注释和 Rustdoc 使用自然、准确的简体中文。保留 Rust 标识符、API 名称和已有英文技术术语。
使用 `//!` 说明模块职责，使用 `///` 说明公开 API 和重要接口，使用 `//` 解释实现意图、约束和
生命周期；不要复述显而易见的语句。每个 `unsafe` 块旁必须有 `// SAFETY:` 注释，具体说明指针、句柄、
线程、生命周期或缓冲区的安全前提，以及代码如何维持这些前提。不得仅为通过 Clippy 而使用 `unsafe`。
首次依赖不直观机制时，解释 Ownership、Borrowing、`Result`、`Option`、`Arc`、`Mutex`、Channel、
`Send`/`Sync`、COM 和 D3D11 资源生命周期。

## 修改公开文档前

先阅读[写作规范](docs/STYLE_GUIDE.zh-CN.md)、[术语表](docs/TERMINOLOGY.zh-CN.md)
及对应英文版本。公开文档默认英文，并提供完整的简体中文版本。
同时更新两种语言、共享修订号和[文档配对目录](docs/document-pairs.json)。

保留 API 名称、线程限制、资源所有权、异常路径和测量边界。
区分事实、提案、估算与实测结果，不把目标写成成绩。
项目概念使用术语表中的名称；新增概念时说明维护模块和生命周期。
不自动修改已批准的架构决策。提出调整时提供依据；重要架构变更获准后，同步更新对应设计文档。

## 隐私和产物

整理原始设计前，先在仓库外备份，不删除原始记录。
内部草稿、机器报告、私人样本、大型测试文件、缓存和构建结果均保存在仓库外。
不发布本地绝对路径、凭据或未经脱敏的日志。
未来本地测试通过 `PIXSTRIDE_STRESS_IMAGE` 提供输入，通过 `PIXSTRIDE_TEST_ROOT` 指定输出根目录；
这两个名称是测试工具的计划约定，尚无对应查看器选项。

## 检查和 Git

修改文档工具后运行 `python -B scripts/check_docs.py` 和
`python -B -m unittest discover -s scripts/tests`；只改文字时运行文档检查。
两种语言仍须人工核对。网络链接检查按需运行：
`python -B scripts/check_docs.py --external-links`。
Rust 开发获准后，修改 Rust 代码须运行适用的 fmt、Clippy、测试和构建检查。
性能改动必须说明验证方法，并区分目标与实测结果。

正常开发从 `main` 创建短期 `feat/*`、`fix/*`、`docs/*`、`refactor/*`、`test/*` 或
`chore/*` 分支，不建立长期 `develop` 分支。流程为检查 → 维护者评审 → Squash Merge，
Agent 不自动合并 PR。首次发布流程见[维护说明](docs/zh-CN/maintenance.md)。
没有负责人明确授权，不推送、不修改 GitHub 设置或可见性、不改写历史，也不删除远程分支。
不把强推或历史改写作为日常任务清理步骤。
管理员工具安装必须先获批准。

## 许可证

负责人已选择 `MIT OR Apache-2.0`，保留 [LICENSE-MIT](LICENSE-MIT) 和
[LICENSE-APACHE](LICENSE-APACHE)。未来 Cargo Package 使用 `license = "MIT OR Apache-2.0"`。
引入依赖或复用代码前检查许可证，原样保留必要声明并标明外部来源。
Pixstride 自身的许可证不替代第三方义务。
