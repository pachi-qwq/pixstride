[English](a1-manual-acceptance.md) | [简体中文](../zh-CN/a1-manual-acceptance.md)

<!-- doc-id: cache-recovery-acceptance; revision: 1; status: active -->

# A1 cache recovery manual acceptance guide

Use the [guided script](../../scripts/accept_a1_cache_recovery.ps1) to check display and interaction.
The script generates fixtures, injects cache faults, checks hit logs and records results; you inspect
what the Viewer displays and confirm each step. This guide is not an acceptance result. Automated tests,
GPU readback and earlier Stage C manual results do not replace this run's confirmation.

## 1. Prepare and start

Use Windows 10/11 x64, PowerShell 7 and a development environment that can already build the project.
See the [build prerequisites](development.md#build-and-run). Close acceptance windows you previously
started. Do not use private images or existing caches.

Open PowerShell 7 at the repository root containing `Cargo.toml`, then run:

```powershell
git rev-parse HEAD
$testParent = Join-Path (Get-Location).Path '..\..\logs\pixstride-a1-acceptance'
$env:PIXSTRIDE_TEST_ROOT = [IO.Path]::GetFullPath($testParent)
New-Item -ItemType Directory -Path $env:PIXSTRIDE_TEST_ROOT -Force | Out-Null
```

Record the commit printed by Git. This example places the test parent under
`logs/pixstride-a1-acceptance`, two levels above the repository. You may replace `$testParent` with
another repository-external directory. Neither it nor its ancestors may be a junction or other
reparse path.

Start manual acceptance:

```powershell
pwsh -NoProfile -File .\scripts\accept_a1_cache_recovery.ps1
```

The console first prints `New isolated acceptance directory:` and this run's directory. Record it;
it is neither the repository nor an old cache. The script builds Release, generates fixtures and runs
two automated capacity regressions before opening a Viewer. Build output goes to log files.

To compile and generate fixtures without manual acceptance, use this command instead:

```powershell
pwsh -NoProfile -File .\scripts\accept_a1_cache_recovery.ps1 -PrepareOnly
```

Do not add `-PrepareOnly` for the actual manual run. Every invocation creates a fresh run directory
and starts from the beginning; it does not resume earlier `PASS` confirmations.

## 2. Identify the fixtures and files

All fixtures are under this run's `fullscreen-gui` directory. You do not supply your own images.

| File | Actual fixture and expected appearance |
|---|---|
| `large.png` | A 20000 × 65 color strip with transparent pixels. A very thin horizontal strip after pressing `F` is expected. At `1`, only part fits; drag horizontally to inspect it. |
| `large-copy.png` | A copy of the previous file with the same pixels but a different source path. Identical appearance at step 03 is expected; check the window title. |
| `a.jpg` | A uniform gray 8 × 6 image, not a photograph. It is tiny at `1`; use `F` or the wheel to enlarge it. |
| `b.png` | An 8 × 6 gray image resembling the JPEG fixture. Confirm the current file through the window title. |
| `c.bmp` | Another similar 8 × 6 gray image. Do not expect a different pattern. |
| `infinite.gif` | A looping 4 × 3 color-block animation. Enlarge it to observe changes; Space pauses and resumes it. |
| `finite.gif` | An additional finite-loop fixture from the generator, not a separately confirmed window in this script. |

Other files in the run directory:

| Path or file | Purpose |
|---|---|
| `build/release/pixstride.exe` | This run's Viewer. The script launches it; do not substitute another repository build. |
| `tiles` | This run's isolated Tile cache, with the 2 GiB default budget. The budget is a limit, not a request to fill it; build output uses additional space. |
| `acceptance.json` | Automated results, confirmed steps and final manual status. Do not edit it to manufacture a pass. |
| `build.log`, `samples.log` | Compilation and fixture-generation logs. |
| Two `.log` files beginning with `a1_` | Automated bad-header and partial-deletion capacity regression logs. |
| Each step's `.stdout.log`, `.stderr.log` | The corresponding Viewer's standard output and error output, including its shutdown report. See section 5 for names. |

## 3. Check each window in order

For every window:

1. Read the console step label and window title to identify the current file.
2. Perform the step's actions and inspect display and interaction.
3. Close the Viewer normally, for example with its close button. Do not close the PowerShell console.
4. Return to the console and enter uppercase `PASS`, then Enter, only if the step behaved correctly.

**Do not use Left/Right or `Ctrl+O` to change files during the first six steps.** Opening an extra
large image changes the cache state and can fail the expected entry-count or cold/warm checks.
Wait until the `07-` steps to test ordinary-image and GIF navigation within the generated directory.

| Console step | File opened automatically | What you do and inspect | What the script checks |
|---|---|---|---|
| `01-cold` | `large.png` | Wait for the color strip. Use `F`, `1`, wheel zoom and left-drag to both ends. Check for obvious seams, incorrect overlapping colors, hangs and failure to close normally. | First construction reports `hit=false`, with exactly one new cache entry. |
| `02-warm` | `large.png` | Reopen, zoom and pan, then close normally. Do not use perceived opening speed as proof of a cache hit. | Reopen reports `hit=true`. |
| `03-other-after-missing-pixels` | `large-copy.png` | The other valid image displays without being blocked by the incomplete old cache. Check that the title names the copy. | First delete this run's original `pixels.bin`; the copy reports `hit=false`, and the incomplete old directory is reclaimed. |
| `04-reopen-original` | `large.png` | The original rebuilds and displays correctly; zoom and pan still work. | The original reports `hit=false`. |
| `05-bad-index-rebuild` | `large.png` | The image recovers normally. An on-screen index-corruption error is not required; the damaged cache should rebuild in the background. | First replace this run's original index with `BADINDEX`, then check `hit=false`. |
| `06-recovered-warm` | `large.png` | The recovered cache reopens and closes normally. | Reopen reports `hit=true`. |
| `07-a.jpg` | `a.jpg` | The gray image displays. Navigate with Left/Right within the generated directory, then return. Check that the title matches the selected file. | Normal Viewer exit and your explicit confirmation. |
| `07-b.png` | `b.png` | The PNG displays. Use `F`, `1`, zoom and pan; do not mistake a tiny gray image for missing content. | Normal Viewer exit and your explicit confirmation. |
| `07-c.bmp` | `c.bmp` | The BMP displays. Switch between it, JPEG and PNG without stale images or hangs. | Normal Viewer exit and your explicit confirmation. |
| `07-infinite.gif` | `infinite.gif` | Color blocks keep changing. Space pauses; Space again resumes. Switch to generated static images and back, then confirm playback and normal close. | Normal Viewer exit and your explicit confirmation. |

The script injects missing pixels and the bad index. Do not manually delete `pixels.bin` or edit
`index.bin`, especially in a private cache. If an ownership check refuses to continue, preserve the
state; do not delete unknown members to bypass it.

## 4. Decide whether the whole run passed

After the last confirmation, the script still checks cache members and actual payload totals.
Entering every `PASS` is not by itself a successful complete run.

Report success only when the script prints this fixed prefix and `acceptance.json` agrees:

```text
Manual GUI Acceptance: Passed (owner-confirmed).
```

The same output line also contains this run's `Managed payload bytes`. Do not expect a fixed byte count.

| `acceptance.json` field | Requirement for a complete pass |
|---|---|
| `AutomatedCapacity` | `Passed`: both automated capacity regressions completed. |
| `ManualGUIAcceptance` | `Passed`: you confirmed every window and the final checks completed. |
| `PassedSteps` | Contains all 10 console steps in the table above. |
| `ManagedPayloadBytes` | The actual verified payload byte count, no greater than 2 GiB. |

The manual result records your confirmations, not automated recognition of the displayed image.
`-PrepareOnly`, `AutomatedCapacity=Passed` alone, green tests or GPU readback do not establish
`ManualGUIAcceptance=Passed`.

## 5. Stop on failure and keep evidence

Do not enter `PASS` for a display, interaction or shutdown problem. If the Viewer still closes
normally, close it and press Enter at the console without typing anything. The script stops with
manual status `Pending`. If the window hangs, record the step and symptoms and preserve the run
directory; do not treat a forced termination as a normal-close pass.

Stop on automated-check errors too. Do not edit JSON, weaken assertions or modify existing private
caches. Use these logs:

| Failure point | Files to retain or inspect |
|---|---|
| Before any window opens | `build.log`, `samples.log`, and whichever of the two `a1_` capacity logs were created. |
| `01-cold` display or exit | `01-cold.stdout.log`, `01-cold.stderr.log`. |
| `03-other-after-missing-pixels` | `03-other-after-missing-pixels.stdout.log`, `03-other-after-missing-pixels.stderr.log`. |
| GIF step | `07-infinite.gif.stdout.log`, `07-infinite.gif.stderr.log`. |
| Another Viewer step | Match the console step label to its `.stdout.log` and `.stderr.log` files. |
| Final cache or capacity check | `acceptance.json`, console error and this run's `tiles` state. Do not clean it first. |

Run the script again to restart. It creates another isolated run directory without overwriting the
previous results. On exit, the script restores the process environment variables it overrides.
Fixtures, caches, build output and logs remain; cleanup is not a condition of acceptance.

## 6. Report the result

Provide the tested commit, final status, failing step if any, and relevant log excerpts to the
maintainer. Keep the full run-directory path local. Remove machine paths, unrelated logs and private
information before posting to a public PR; do not upload the whole cache or build directory.
Passing manual acceptance does not mean the PR has been merged.

- Read [Stage C's A1 recovery boundaries](stage-c.md#a1-cache-recovery-hardening) for ownership, accounting and unverified stress scenarios.
- Use the [development setup and check commands](development.md) to diagnose build prerequisites; do not elevate privileges or change protection settings to make acceptance pass.
