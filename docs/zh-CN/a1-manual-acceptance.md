[English](../en/a1-manual-acceptance.md) | [简体中文](a1-manual-acceptance.md)

<!-- doc-id: cache-recovery-acceptance; revision: 1; status: active -->

# A1 缓存恢复人工验收指南

使用[引导脚本](../../scripts/accept_a1_cache_recovery.ps1)逐项检查画面和交互。
脚本负责生成样本、注入缓存故障、核对命中日志和记录结果；你负责目视检查并逐项确认。
本指南不是验收通过记录。自动化测试、GPU 回读和以前的 Stage C 人工结果都不能代替本次确认。

## 1. 准备和启动

使用 Windows 10/11 x64、PowerShell 7，以及已能构建项目的开发环境。
工具链要求见[开发指南](development.md#构建和运行)。关闭之前由你启动的验收窗口，不使用私人图片或既有缓存。

在包含 `Cargo.toml` 的仓库根目录打开 PowerShell 7，执行：

```powershell
git rev-parse HEAD
$testParent = Join-Path (Get-Location).Path '..\..\logs\pixstride-a1-acceptance'
$env:PIXSTRIDE_TEST_ROOT = [IO.Path]::GetFullPath($testParent)
New-Item -ItemType Directory -Path $env:PIXSTRIDE_TEST_ROOT -Force | Out-Null
```

记下输出的提交号。示例测试父目录位于仓库向上两级的 `logs/pixstride-a1-acceptance` 下；
也可以将 `$testParent` 改为自己选择的仓库外目录。目录及其祖先不能是 junction 或其他重解析路径。

开始人工验收：

```powershell
pwsh -NoProfile -File .\scripts\accept_a1_cache_recovery.ps1
```

终端先输出 `New isolated acceptance directory:` 及本轮目录。记下该目录，它不是仓库目录，也不是旧缓存。
脚本先编译 Release、生成样本，再执行两项自动化容量回归。这个阶段尚未弹出 Viewer，构建输出写入日志。

只想预先编译和生成样本时，可以改用以下命令，但它不进行人工验收：

```powershell
pwsh -NoProfile -File .\scripts\accept_a1_cache_recovery.ps1 -PrepareOnly
```

正式验收不要加 `-PrepareOnly`。再次运行会创建新的独立目录，从头开始，不会续用上一轮的 `PASS`。

## 2. 先认清样本和文件

所有样本都在本轮目录的 `fullscreen-gui` 中，不需要自己准备图片。

| 文件 | 实际样本与正常外观 |
|---|---|
| `large.png` | 20000 × 65 的超宽彩色条，含透明像素。按 `F` 后显示成很细的横条属于正常；按 `1` 后只能看到其中一段，需要左右拖动。 |
| `large-copy.png` | 上一文件的副本，画面相同，但源文件路径不同。第 03 步显示相同画面不代表开错文件，检查窗口标题。 |
| `a.jpg` | 8 × 6 的均匀灰色小图，不是照片。按 `1` 时很小，按 `F` 或滚轮放大后检查。 |
| `b.png` | 与 JPEG 样本相似的 8 × 6 灰色图，切换后用窗口标题确认文件。 |
| `c.bmp` | 与前两者相似的 8 × 6 灰色图，不应期待不同的图案。 |
| `infinite.gif` | 4 × 3 的循环色块动画。放大后观察色块变化，用空格暂停和继续。 |
| `finite.gif` | 生成器附带的有限循环样本，不是本轮脚本要求单独确认的窗口。 |

本轮目录中的其他文件：

| 路径或文件 | 用途 |
|---|---|
| `build/release/pixstride.exe` | 本轮编译的 Viewer；脚本自动启动，不要误用仓库中其他构建版本。 |
| `tiles` | 仅供本轮 Viewer 使用的 Tile 缓存，默认预算为 2 GiB；预算不是要求写满的容量，构建输出另占空间。 |
| `acceptance.json` | 自动化结果、已确认步骤及最终人工状态。不要手工修改来补成通过。 |
| `build.log`、`samples.log` | 编译和样本生成日志。 |
| 两个以 `a1_` 开头的 `.log` 文件 | 坏索引头及部分删除失败的自动化容量回归日志。 |
| 每步的 `.stdout.log`、`.stderr.log` | 对应 Viewer 退出时的标准输出和错误输出。具体命名见第 5 节。 |

## 3. 按顺序检查每个窗口

每个窗口均按以下顺序完成：

1. 查看本步终端标签和窗口标题，确认正在检查的文件。
2. 完成本步操作，判断画面和交互是否正常。
3. 正常关闭 Viewer 窗口，例如点击右上角关闭按钮。不要关闭 PowerShell 终端。
4. 回到终端，只在确认正常后输入大写 `PASS`，再按回车。

**前六步不要用左右方向键或 `Ctrl+O` 提前切图。** 额外打开大图会改变本轮缓存状态，
可能让脚本的条目数量或冷暖命中检查失败。等进入 `07-` 步骤后，再检查生成目录内的普通图片和 GIF 导航。

| 终端步骤 | 自动打开的文件 | 你要操作和观察的内容 | 脚本自动核对 |
|---|---|---|---|
| `01-cold` | `large.png` | 等待出现彩色条；按 `F`、`1`，滚轮缩放，左键拖到左右两端。无明显接缝、异常叠色或卡死，能够正常关闭。 | 首次构建为 `hit=false`，且只有一条新建缓存。 |
| `02-warm` | `large.png` | 再次显示正常，重复缩放和平移后正常关闭。不以“打开得快”作为缓存命中的证据。 | 重新打开为 `hit=true`。 |
| `03-other-after-missing-pixels` | `large-copy.png` | 另一张有效图能正常显示，不被缺文件的旧缓存阻塞。确认标题是副本文件。 | 先删除本轮原图缓存的 `pixels.bin`；副本为 `hit=false`，旧残缺目录随后被回收。 |
| `04-reopen-original` | `large.png` | 原图能够重新构建并正常显示、缩放和平移。 | 原图为 `hit=false`。 |
| `05-bad-index-rebuild` | `large.png` | 画面恢复正常，不要求屏幕显示“索引损坏”错误。损坏缓存应在后台重建。 | 先将本轮原图索引改为 `BADINDEX`，随后检查 `hit=false`。 |
| `06-recovered-warm` | `large.png` | 恢复后的缓存再次打开正常，能够正常关闭。 | 再次打开为 `hit=true`。 |
| `07-a.jpg` | `a.jpg` | 灰色图块显示正常；在生成目录内用左右键切图，再切回来。检查标题与当前文件一致。 | Viewer 正常退出，且你明确确认。 |
| `07-b.png` | `b.png` | PNG 显示正常；按 `F`、`1`，检查缩放和平移，不把小灰块误判为缺图。 | Viewer 正常退出，且你明确确认。 |
| `07-c.bmp` | `c.bmp` | BMP 显示正常；与 JPEG、PNG 往返切换，不残留上一张图或卡死。 | Viewer 正常退出，且你明确确认。 |
| `07-infinite.gif` | `infinite.gif` | 色块持续变化；空格暂停，再按空格继续。可与生成的静态图往返切换，返回后再确认播放和正常关闭。 | Viewer 正常退出，且你明确确认。 |

缺文件和坏索引都由脚本自动注入。不要自己删除 `pixels.bin`、修改 `index.bin`，更不要对私人缓存执行这些操作。
脚本的归属检查拒绝继续时，保留现场，不要删除未知成员来绕过检查。

## 4. 判断整轮是否通过

最后一个窗口确认后，脚本还会核对缓存成员和实际载荷总量。因此，输入完所有 `PASS` 不等于整轮已经通过。

只有脚本最终输出以下固定前缀，且 `acceptance.json` 中的状态一致，才能报告本轮通过：

```text
Manual GUI Acceptance: Passed (owner-confirmed).
```

实际输出在同一行还会包含本轮的 `Managed payload bytes`，不要期待某个写死的字节数。

| `acceptance.json` 字段 | 完整通过时的要求 |
|---|---|
| `AutomatedCapacity` | `Passed`，表示两项自动化容量回归完成。 |
| `ManualGUIAcceptance` | `Passed`，表示全部窗口经你确认且最后检查完成。 |
| `PassedSteps` | 包含上表 10 个终端步骤。 |
| `ManagedPayloadBytes` | 本轮核对得到的实际载荷字节数，不超过 2 GiB。 |

这里的人工结果是你的逐项确认，不是脚本自动识别了画面。`-PrepareOnly`、只有 `AutomatedCapacity=Passed`、
测试全绿或 GPU 像素回读通过，都不能记作 `ManualGUIAcceptance=Passed`。

## 5. 失败时停止并保留证据

画面、交互或正常关闭有问题时，不输入 `PASS`。如果 Viewer 还能正常关闭，先关闭它，再在终端直接按回车；
脚本会结束本轮，人工状态保持 `Pending`。如果窗口卡住，记录当前步骤和现象，保留本轮目录，不把强制结束当成正常关闭通过。

自动检查报错也要停止，不修改 JSON、不放宽断言、不操作已有私人缓存。可按下表定位日志：

| 失败位置 | 需要保留或查看的文件 |
|---|---|
| 还没有窗口就失败 | `build.log`、`samples.log`，以及两个 `a1_` 容量回归日志中已生成的文件。 |
| `01-cold` 显示或退出异常 | `01-cold.stdout.log`、`01-cold.stderr.log`。 |
| `03-other-after-missing-pixels` 失败 | `03-other-after-missing-pixels.stdout.log`、`03-other-after-missing-pixels.stderr.log`。 |
| GIF 阶段失败 | `07-infinite.gif.stdout.log`、`07-infinite.gif.stderr.log`。 |
| 其他 Viewer 步骤失败 | 按终端步骤名寻找同名的 `.stdout.log`、`.stderr.log`。 |
| 最后的缓存或容量检查失败 | `acceptance.json`、终端报错及本轮 `tiles` 目录现场；不要先清理。 |

重新开始时，重新运行脚本即可。它会创建新的独立运行目录，不会覆盖旧一轮的结果。
脚本退出时恢复自身覆盖的进程环境变量；样本、缓存、构建输出和日志保留，不要求你清理目录才能完成验收。

## 6. 提交验收结果

向维护者提供测试提交号、最终状态、失败步骤（若有）和相关日志片段。
本轮目录的完整路径留在本地；公开 PR 前移除机器路径、无关日志和私人信息，不上传整个缓存或构建目录。
人工验收通过也不表示 PR 已合并。

- 查看 [Stage C 的 A1 恢复边界](stage-c.md#a1-缓存恢复加固)，理解未知归属、实际计费及未验证的压力场景。
- 查看[开发环境与检查入口](development.md)，排查编译前置条件；不要为通过验收而提升权限或修改保护设置。
