[English](STYLE_GUIDE.md) | [简体中文](STYLE_GUIDE.zh-CN.md)

<!-- doc-id: style; revision: 1; status: active -->

# Documentation style guide

Write so a reader can understand the design, its limits, and the evidence needed to evaluate it.
Accuracy takes precedence over brevity. This guide applies to public prose; API syntax and quoted
source material keep their original spelling.

## Language and evidence

English is the default public language. Provide a complete Simplified Chinese counterpart using
[the document pair catalog](document-pairs.json). Translate meaning, requirements, units, versions,
and uncertainty together. Neither language may introduce a stronger promise. Internal notes may remain
Chinese and stay outside the public tree.

Distinguish four kinds of statement: documented API constraints, design proposals, calculated estimates,
and measured results. Cite primary sources near external technical claims. For measurements, give
the build, conditions, sample count, percentile, and exact start/end events. Preserve failed cases and
unknowns. Never add numbers to make a rewrite sound specific.

## English prose

- State the problem or action early. Prefer concrete verbs and name the responsible module.
- Use active voice when it clarifies ownership. A buffer, decoder, or thread may be the subject.
  Passive voice is acceptable when the actor is unknown or irrelevant.
- Use *must* for requirements, *should* for recommendations, and *may* for options.
  Keep words such as *usually*, *estimated*, and *not yet verified* when they express real limits.
- Remove promotional adjectives, staged openings, forced contrasts, and repeated summaries.
  Do not replace a complete explanation with a slogan or an unexplained list of nouns.
- Use numbered steps for order and bullets for parallel items. A real three-step procedure needs no
  artificial change in length. No rule bans all adverbs, questions, parentheses, or three-item lists.

## Simplified Chinese prose

Use modern, natural Chinese. Prefer explicit verbs and short clauses, with one question or decision
per paragraph. Avoid nominal phrases that hide the action or owner. Preserve the explanation a reader
needs, even when it makes a paragraph longer.

Review empty phrases such as “赋能”, “抓手”, and unsupported claims of improvement. Review
staged contrasts, formulaic transitions, forced parallel sentences, and repeated conclusions in context.
Do not turn these examples into a blanket word ban. A technical comparison or a description of a
closed control loop may be necessary. Delete filler, not qualifications or evidence.

Use Chinese punctuation in Chinese sentences and ASCII punctuation in complete English sentences,
code, identifiers, and URLs. Separate Chinese from Latin words and numbers with a space, including
around inline code and link labels. Do not insert spaces inside an official product name or identifier.
Write `512 MiB`, `2 ms`, `90°`, and `25%`; distinguish GiB from decimal GB. Treat line breaks as
layout, not permission to change a unit, equation, or measured value.

## Terms and structure

Use [TERMINOLOGY](TERMINOLOGY.md). On first use, give a short explanation suitable for the audience;
in Chinese, use the Chinese name followed by the English term when it helps. Keep API names in code.
Do not translate `ID3D11DeviceContext` or alter its case.

For a project concept, define what it represents, why it exists, who owns it, when it is created,
updated and released, and how it differs from nearby concepts. Reuse an existing concept where possible.

A design should explain the problem, chosen flow, rationale, limits, failure behavior, and validation.
Choose headings for the content instead of repeating a fixed template on every page. Use Mermaid for
thread/data/lifetime relationships when it saves explanation, and describe the diagram in prose.
Each figure, table, and assertion must agree with the text and the implementation status.

## Markdown

- Use UTF-8, LF, a final newline, one H1, and ATX headings without skipped levels.
- Leave blank lines around headings, lists, tables, and fenced blocks. Give fenced blocks a language.
- Use descriptive relative links within the repository and HTTPS links for external references.
  Do not link public pages to private drafts, machine reports, or local absolute paths.
- Wrap prose near 100 characters when practical. Do not hard-wrap URLs, API identifiers, or tables.
- Put commands and identifiers in backticks. Use tables for comparisons, not paragraph layout.
- Keep English and Chinese entries linked at the top. Update their revision metadata together.

## Rewrites from Pixstride's early drafts

The excerpts below come from the preserved architecture, rendering, performance, and decoder drafts.
They contain no private paths. They illustrate readability work, not changes to technical decisions.
The Chinese guide includes natural Chinese rewrites of the same excerpts.

### 1. Explain routing rather than compressing the rule

Original excerpt:

> 普通图片不经过 Tile Builder。超大图片也不先分配全图 Bitmap 再尝试分块。
> 路由依据头信息、预测像素字节、单纹理限制和资源预算，不能只看压缩文件大小。

Revised:

> Keep ordinary images on the direct decode-and-upload path. Select the large-image path using header
> dimensions, estimated decoded bytes, texture limits, and available budgets. That path must avoid a
> full-image bitmap allocation; compressed file size alone is insufficient for routing.

The rewrite introduces the routing decision and keeps all four inputs and the allocation constraint.

### 2. Keep the threading tradeoff and the experiment

Original excerpt:

> 这不是“多线程一定更快”的结论。选择的依据是隔离驱动阻塞与 UI 响应，代价是一次唤醒。
> A 阶段测 GPU 命中端到端延迟，若线程切换显著损害 P95，先优化合并与唤醒，再重新评审。

Revised:

> A render thread isolates driver stalls from UI input handling and adds a thread wakeup. Stage A will
> measure end-to-end latency for GPU cache hits. If the extra thread worsens P95 latency materially,
> investigate notification coalescing and wakeups, then review the thread model.

The reader sees the benefit, cost, and condition for reconsideration without a staged contrast.

### 3. Name what an upload budget controls

Original excerpt:

> 每轮上传初值 ≤16 MiB 或 CPU 提交预算 2 ms，先服务最新目标，再做预取。
> 这是停止提交更多任务的软预算，**不能抢占一个已经阻塞的驱动调用**。

Revised:

> Initially, stop submitting uploads when a round reaches 16 MiB or 2 ms of CPU submission time.
> Submit work for the latest target before prefetch work. This soft limit cannot interrupt a driver
> call that has already blocked.

The limit now has an actor and a stop condition. The uninterruptible-call exception remains explicit.

### 4. Preserve the difference between processing and display

Original excerpt:

> 50 ms窗口目标不能用“调用了ShowWindow”就判通过，外部窗口可见与首个合成帧要交叉验证。
> 缓存命中5 ms是处理目标，60 Hz屏幕无法保证每次实际屏显小于5 ms。

Revised:

> Validate the 50 ms window-visibility target with an external visibility observation and the first
> composed frame; a `ShowWindow` call is insufficient evidence. The 5 ms cache-hit target measures
> application processing. It does not guarantee a display change within 5 ms on a 60 Hz display.

The rewrite separates two measurement boundaries and keeps both values as targets.

### 5. Explain the PNG dependency before its consequence

Original excerpt:

> 反过滤可能依赖上一行，Deflate还依赖滑动窗口；不能用`文件偏移+矩形`直接读任意区域。

Revised:

> PNG unfiltering may need the previous row, and Deflate needs its sliding-window state. A file offset
> and rectangle are therefore insufficient for arbitrary region decoding.

The dependency is explicit. The rewrite does not turn incremental input into random region access.

### 6. Keep cancellation values provisional

Original excerpt:

> 取消每若干行/每个I/O块检查，目标协作退出≤100 ms；500 ms无回应可终止worker，
> 这些也是测试目标。

Revised:

> Check cancellation between row batches and I/O blocks. The initial target is cooperative exit within
> 100 ms. A worker that has not responded after 500 ms may be terminated. Both thresholds need testing.

The rewrite separates the action, soft target, and fallback without implying a measured guarantee.

## Review before publication

- Can the intended reader follow the flow and find the owner of each resource?
- Are API names, limits, units, versions, failure paths, and uncertainty unchanged?
- Do both languages state the same requirements and measurement boundaries?
- Are estimates and future behavior clearly separate from measured results and current behavior?
- Have private paths, credentials, local logs, and unapproved media been removed?
- Do links, Markdown, pair metadata, and tooling checks pass? Has a person reviewed warnings and prose?

See [writing sources and license review](en/writing-sources.md) for adopted principles and exceptions.
Automatic checks catch specific defects; they do not certify readability or factual correctness.
