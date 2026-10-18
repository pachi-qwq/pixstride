[English](../en/writing-sources.md) | [简体中文](writing-sources.md)

<!-- doc-id: writing-sources; revision: 1; status: reviewed-reference -->

# 写作来源与复用决定

阅读下列项目的核心规则、示例和许可声明后，为 Pixstride 单独编写规范。
记录固定为 2026-10-18 观察到的 commit。本仓库没有打包或安装上游 Skill、短语词典或第三方示例。

| 参考项目 | 核查版本与许可 | 采用和调整 |
|---|---|---|
| [hardikpandya/stop-slop](https://github.com/hardikpandya/stop-slop/tree/8da1f030185bdfe8471220585162991eaeb970e9) | MIT，copyright 2025 Hardik Pandya。[许可](https://github.com/hardikpandya/stop-slop/blob/8da1f030185bdfe8471220585162991eaeb970e9/LICENSE)。 | 采用具体表达、去除刻意修辞，不全面禁止副词/被动语态/疑问句，也不将解释改成口号。 |
| [Gawg-AI/stop-slop-cn](https://github.com/Gawg-AI/stop-slop-cn/tree/bdbd9cceaec791f2bd280b4a387a69be09b742d6) | MIT，许可只写 copyright 2025，没有具名持有人。[许可](https://github.com/Gawg-AI/stop-slop-cn/blob/bdbd9cceaec791f2bd280b4a387a69be09b742d6/LICENSE)。 | 优先用动词和具体主语，检查模板表达；保留不确定性，不补造数字、不强迫句子节奏、不自动加强暂定结论。 |
| [ruanyf/document-style-guide](https://github.com/ruanyf/document-style-guide/tree/571951731efb4b83b1939af1d3dd830441ebef45) | README 声明公共领域，核查的文件树没有独立 LICENSE。[声明](https://github.com/ruanyf/document-style-guide/blob/571951731efb4b83b1939af1d3dd830441ebef45/README.md)。 | 采用清楚的标题、段落、标点和数值单位，不要求短设计页套用整本手册的章节。 |
| [sparanoid/chinese-copywriting-guidelines](https://github.com/sparanoid/chinese-copywriting-guidelines/tree/2c2d89c8c7cf8c2b96509e58626e73f6bc223347) | MIT，copyright 2023 Sparanoid, Inc.。[许可](https://github.com/sparanoid/chinese-copywriting-guidelines/blob/2c2d89c8c7cf8c2b96509e58626e73f6bc223347/LICENSE)。 | 统一中英文空格和标点，保留标识符、单位、URL 和官方名称；主观排版偏好只作评审建议。 |

阅读范围包括两个 stop-slop 项目的 `SKILL.md` 和改写示例，技术文档规范的文本、标题、段落、
数字、标点和文档组织，以及 Sparanoid 的空格、标点、名称和争议章节。
来源快照保存在内部，便于核查。

## 项目级 Skill

项目 Skill 可以引用同一套规范，但 [AGENTS.md](../../AGENTS.md) 已提供跨 Agent 的明确入口，
现阶段额外安装收益有限，因此没有安装项目 Skill。以后增加平台适配器时，也不能让关键规则
只能通过该适配器访问。

以后如果复制上游文件或实质文本，需要核实固定版本的许可，保留版权和许可声明，并按要求
记录修改和来源。只保留参考链接不能代替这些义务。
上游许可适用于各自作品，不决定 Pixstride 的项目许可证。
项目规范中的改写示例全部来自已保留的 Pixstride 原稿，保留原有技术限定条件。
