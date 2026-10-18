[English](CONTRIBUTING.md) | [简体中文](CONTRIBUTING.zh-CN.md)

<!-- doc-id: contributing; revision: 2; status: active -->

# 贡献指南

Pixstride 仍处于设计评审阶段，目前接受文档、评审提案、仓库配置、CI 和文档工具方面的改动。
查看器实现需要单独批准的任务。

## 准备改动

阅读[写作规范](docs/STYLE_GUIDE.zh-CN.md)、[术语表](docs/TERMINOLOGY.zh-CN.md)
和[开发指南](docs/zh-CN/development.md)。引入依赖或扩展设计前，说明问题和预期结果。
保留 API 限制、估算、提案和实测行为之间的区别。

初始 `main` 发布后，从最新 `main` 创建短期分支：

```sh
git switch main
git pull --ff-only
git switch -c docs/describe-change
```

按改动类型使用 `feat/*`、`fix/*`、`docs/*`、`refactor/*`、`test/*` 或 `chore/*`，
不建立长期 `develop` 分支。空仓库初始化流程见[维护说明](docs/zh-CN/maintenance.md)，
上述命令不能替代尚未建立的初始历史。

## 文档和检查

英文与简体中文页面一起更新，并在[配对目录](docs/document-pairs.json)中保持相同文档 ID 和修订号。
模板可以在同一文件中提供两种语言。新增术语应说明定义、维护模块和生命周期，
API 名称和单位保持准确拼写。

```sh
python -B scripts/check_docs.py
python -B -m unittest discover -s scripts/tests
```

修改文档工具时运行第二条命令，它测试检查工具，不测试查看器。
网络链接检查按需运行 `python -B scripts/check_docs.py --external-links`。
提示、翻译、API 事实和可读性仍需人工核对。脚本通过不能证明实现达到了性能目标。

## 提交 Pull Request

每个 PR 集中解决一个问题，说明最终行为、验证依据和剩余风险。
使用现有模板，报告已执行、未执行的检查及原因。推荐 Conventional Commits，
例如 `docs: define stage A ownership`，不要求安装提交钩子框架。

正常流程为检查 → 维护者评审 → Squash Merge → 删除已合并功能分支。
个人维护者可以自审后通过 PR 合并，不强制其他人批准，required approvals 应为 0。
日常贡献不强推共享历史，也不顺带修改远程设置。Agent 推送或修改远程设置前必须取得负责人明确授权。

不把私人图片、大型生成文件、机器报告、本地绝对路径或凭据放入提交和 Issue 附件。
忽略已跟踪文件不能将它移出历史：发布前暂停，检查暴露范围，并取得具体的处理决定。
不自动改写历史，详见[发布前检查](docs/zh-CN/maintenance.md#发布前检查)。

## 许可证与资源

Pixstride 采用 MIT 或 Apache-2.0 双许可证，使用者可以自行选择其中一种许可证。
条款见 [LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)，SPDX 表达式为
`MIT OR Apache-2.0`。除非明确另作声明，有意提交到项目的贡献采用相同条款。
未来 Cargo Package 使用 `license = "MIT OR Apache-2.0"`。

引入依赖或复用代码前确认授权条件，原样保留必要的第三方版权和许可证声明。
标明外部成果来源，不将其当作 Pixstride 原创代码。项目许可证不覆盖第三方组件的义务。

合法授权的小型图片经来源、元数据、隐私和大小评审后，可以放在 `assets/` 或 `tests/fixtures/`。
检查器允许这两个目录中的图片不超过 1 MiB，且不会解码图片；工具不能批准其内容或许可证。
私人图片、大型生成文件和极限样本不进入 Git。
