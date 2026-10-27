[English](stage-d3a2.md) | [简体中文](../zh-CN/stage-d3a2.md)

<!-- doc-id: borderless-fullscreen; revision: 5; status: fullscreen-review -->

# D3-a2: borderless fullscreen and window restoration

This local branch adds F11 borderless fullscreen and Esc restoration to merged D3-a1 geometry.
Automated verification passed. The owner reported normal operation in the exercised fullscreen cases
and passed all five build-progress checks. Uncovered display/DPI scenarios remain unverified;
PR review and merge are still pending.
The implementation reuses the existing HWND, Device, Immediate Context, Swap Chain and Render Thread.
It changes neither display modes nor refresh rates and never calls `SetFullscreenState(TRUE)`.

## Window state and ownership

The UI thread owns `window::fullscreen::State` inside the existing `WindowContext`. Windowed state
includes native normal/maximized placement. A single entry snapshot holds GWL_STYLE, GWL_EXSTYLE,
WINDOWPLACEMENT and the entry monitor's rectangle/device name. It is retained through fullscreen,
minimize/restore and display changes, then cleared only after successful restoration. This snapshot
is in memory for the current window; this stage does not add persistent configuration.

Entering and Exiting mark synchronous transactions. Fullscreen is committed only after the native
operations and actual monitor-sized rectangle succeed. A failed entry attempts one restoration; a
failed exit attempts one return to borderless. If both operations fail, Recovery retains the original
snapshot for another F11/Esc restoration attempt. Errors go to stderr without a modal dialog.
No retry loop or replacement HWND is created.

`SetWindowPos` and `SetWindowPlacement` can synchronously reenter the window procedure. The transaction
handler runs before its ordinary mutable context borrow; each raw-pointer field access ends before
calling a window API. Transition states suppress nested fullscreen transactions. The existing
WM_NCDESTROY and Render/Worker completion sequence still governs destruction.

## Entry, exit and input

Entry saves the native state, uses `MonitorFromWindow` (largest intersection, nearest fallback), and
reads `GetMonitorInfoW`. It removes the overlapped frame/maximized style and only extended frame bits,
then calls `SetWindowPos` over **rcMonitor**, including the taskbar area, with SWP_FRAMECHANGED.
The rcWork rectangle remains reserved for windowed recovery. Frame refresh follows the
[SetWindowPos contract](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos).
It preserves activation, owner and Z order; it never adds Topmost or changes Explorer settings.

The Swap Chain's own Factory explicitly uses `MakeWindowAssociation` with `DXGI_MWA_NO_ALT_ENTER`
and `DXGI_MWA_NO_WINDOW_CHANGES`, keeping automatic DXGI mode handling out of the UI's window
transactions. The baseline did not set these flags; this is a correctness guard for the windowed-only
design, not a reproduced exclusive-mode failure or a timing optimization. The GPU probe sends
Alt+Enter to its own HWND and queries `GetFullscreenState`; the Swap Chain stays windowed before
and after Alt+Enter and each F11 case. See [MakeWindowAssociation](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgifactory-makewindowassociation).

Exit restores the saved style and the extended frame bits it modified, calls `SetWindowPlacement`
with the saved WINDOWPLACEMENT, and refreshes the non-client frame without another move/resize.
Normal position, normal dimensions and maximized state come from native placement rather than a
second application position model. Workspace coordinates remain exclusively in the placement API;
they are never passed to `SetWindowPos`. See [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement)
and [SetWindowPlacement](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowplacement).

F11 toggles fullscreen. Its auto-repeat messages are consumed without toggling again. Esc restores
only an active fullscreen/recovery state; it does not add a windowed exit command. Direction-key
release, F/1, Ctrl+O and Space keep their existing semantics. Alt+F4 uses `DefWindowProc` and the
ordinary asynchronous WM_CLOSE path. Starting a fullscreen transaction cancels a current drag.

## Displays, DPI and view continuity

Screen coordinates can be negative. Fullscreen uses the selected monitor's physical screen rectangle,
while WM_SIZE queries the actual client dimensions with `GetClientRect`, avoiding its packed 16-bit
size fields. Minimized windows publish zero dimensions. Duplicate dimensions publish no new render work.
Per-Monitor V2 awareness and the existing one-image-pixel-per-physical-client-pixel 100% rule remain.

During fullscreen, WM_DPICHANGED uses the current monitor bounds rather than the windowed suggested
rectangle. WM_DISPLAYCHANGE, WM_MOVE and restore-size notifications realign fullscreen to the nearest
current monitor; an unchanged rectangle makes no positioning call. Minimized windows stay minimized.
On exit, native placement handles obsolete off-screen positions. If the saved monitor identity no longer matches its entry rectangle, a normal window is additionally fitted inside the nearest current work area, shrinking
only if needed. Negative/large coordinate arithmetic uses checked dimensions and i64 intermediates.
Actual monitor removal and mixed-DPI hardware combinations remain unverified.

Resize continues through D3-a1: preserve extra Zoom and the source point under the old client center,
then place that point under the new center. Fit recomputes its base scale; 100% remains physical pixels.
Fullscreen is not a file change: it does not advance Generation, reset view, clear image caches,
restart a GIF Session, reset Loop progress or change Delay scheduling. Actual minimization and modal
file-dialog flags retain D1-b2's existing pause rules. Tile LOD/cache format and all resource budgets
remain unchanged: ordinary single-image 256 MiB, shared CPU/GPU 512 MiB each, and existing subquotas.

## Verification and measurements

Fixtures use `PIXSTRIDE_TEST_ROOT` outside Git. The private extreme PNG and its existing cache were untouched.
The complete Rust suite passes 108 tests; 40 explicit diagnostics remain ignored by default.
All 23 selected Windows Release diagnostics pass when run separately, including six new fullscreen
probes/preparation commands and the 17 D3-a1 regression probes.

| Check | Result |
|---|---|
| Native transactions | Normal/resized/maximized restoration; F11 repeat/Esc; same-rectangle notification deduplication; actual fullscreen client size; injected style/placement/position/frame failures, rollback and retry. |
| Display fallback | Negative/overflow/zero geometry, simulated lost monitor identity and obsolete placement, and native movement/DPI notifications. These do not prove physical hotplug or DPI migration. |
| GPU output | 33,644,544 pixels across eight actual window/fullscreen Swap Chain cases, independent bilinear/alpha oracle, channel tolerance 2. Device, Swap Chain and SRV identities stay unchanged; static fixture payload is 16 bytes. |
| Viewer | 12 static and eight playing-GIF transitions, observed-center continuity, no extra static decode/upload, stable Session/Generation, pause/minimize/dialog flags, finite-loop stability, GIF/ordinary/Tile navigation and fullscreen close during playback. |
| Navigation | Separate fullscreen ordinary/GIF slow-worker hold/reverse/KeyUp probes end with target and presented Generation both 61. |
| Idle/resources | Gated static/paused/finished observations add no Present or render wake. Forty warmed native round trips retain the same process/GDI/USER handle counts. |

The native timing probe measures synchronous key dispatch through the completed UI transaction,
including generated native messages, without a Renderer. A separate Viewer probe measures real
Swap Chain Resize and GIF deadlines. Two verification runs on one machine are recorded below;
these are ranges of per-run percentiles/maxima before the Factory guard, not pooled samples or a baseline comparison.
The environment is Windows, RTX 4080 SUPER, Feature Level 11.1, Rust 1.96.0, Release/locked,
with a 3840×2160 display at DPI 168 (175%). A synthetic DPI-144 message does not establish a real 150% test.

| Metric | Samples per run | P50 / P95 / maximum ranges (ms) |
|---|---|---|
| UI entry transaction | 40 | 0.494–0.724 / 3.449–13.153 / 4.240–18.816 |
| UI exit transaction | 40 | 0.881–1.276 / 3.686–14.832 / 3.840–19.970 |
| Viewer Swap Chain Resize | 36 | 1.086–1.103 / 1.690–1.954 / 1.769–2.280 |
| Viewer GIF Deadline Lateness | 25 | 5.137–5.373 / 12.773–14.379 / 15.039–15.424 |
| Separate ordinary warm GPU hit → Present return | 110 | 0.235–0.253 / 6.594–6.731 / 7.095–7.164 |

The native probe's process/GDI/USER counts remain 144/2/4 before and after its warmed loop.
The Viewer closes during fullscreen GIF playback and completes cleanup in 19.388/22.844 ms (one sample per run).
Final animation bytes, shared CPU bytes, Credits and queued outputs are zero. Its tiny fixtures report
ordinary CPU/GPU peaks of 672/720 bytes, animation peaks of 192/48 bytes and Credit peak 2;
Tile CPU/GPU peaks are 1,374,840/1,387,380 bytes. Separate Tile regression reaches
63,197,532/93,206,000 bytes within existing limits. These are reported payload categories, not additive
process/VRAM totals; Swap Chain, WIC, driver and OS allocations are not isolated by these counters.
The later verification used the same production code and input sequence after a test-only assertion
addition. Desktop scheduling was not isolated; the native transaction P95 variation is not attributed.
The small workloads and two timing runs do not establish stability, latency equivalence or performance improvement.

After adding the Factory guard, all 23 diagnostics pass again, including actual windowed-state
queries and the same independent pixel oracle. This separate final verification records:

| Metric | Samples | P50 / P95 / maximum (ms) |
|---|---|---|
| UI entry transaction | 40 | 0.503 / 2.044 / 9.011 |
| UI exit transaction | 40 | 0.911 / 3.006 / 3.188 |
| Viewer Swap Chain Resize | 37 | 1.075 / 1.464 / 1.498 |
| Viewer GIF Deadline Lateness | 26 | 4.095 / 14.665 / 15.421 |
| Separate ordinary warm GPU hit → Present return | 110 | 0.264 / 6.632 / 7.248 |

Cleanup takes 22.771 ms (one sample); warmed process/GDI/USER counts remain 144/2/4.
These verification rounds are not an A/B experiment: desktop timing is not isolated and actual
Resize/presentation sample counts vary with message coalescing. The native probe does not create
a Renderer. The guard was added for mode correctness; the numbers do not establish a timing benefit.

All durations end at application-side events. Present return is not displayed-frame completion or
Input-to-Photon. There is no new attribution or optimization claim for [Issue #11](https://github.com/pachi-qwq/pixstride/issues/11);
the original 0.394–0.905 ms static GPU-hit P95 difference and DXGI admission fix remain intact.

## Windows manual acceptance and remaining coverage

The project owner previously reported that the other exercised fullscreen cases worked normally,
while first-build progress was unclear. The progress fix has now passed the five manual checks below.
The earlier general feedback is retained without converting it into a per-item or all-hardware pass.
The original fullscreen checklist remains available for explicit coverage confirmation.
Automated self-owned-window tests do not
establish visual flicker, taskbar/Alt+Tab behavior, or coverage of every physical display combination.

1. F11 enters/exits borderless fullscreen; holding F11 does not oscillate.
2. Esc exits fullscreen and does not close a windowed viewer.
3. A resized normal window returns to its previous position and dimensions.
4. A maximized window returns maximized; restoring it keeps its earlier normal placement.
5. Fit/100%, zoom and pan retain the observed region through fullscreen and windowed transitions.
6. Playing/paused GIFs and finite/infinite Loop states remain correct.
7. Hold/reverse navigation and Ctrl+O/cancel work in fullscreen.
8. Alt+Tab, Alt+F4 and minimize/restore work normally.
9. Repeated quick F11 transitions have no persistent black frame, hang or unacceptable flicker.
10. Ordinary images and a generated large PNG show correct corners, center and Tile seams.
11. Where equipment permits, check 100%/150%/200%, mixed DPI, different resolutions, negative coordinates and monitor removal.
12. Closing fullscreen leaves no orphan process or apparent resource growth.

The progress follow-up adds [phase and row/Tile counts](stage-c.md#first-build-feedback-on-the-d3-a2-branch)
to the title and console, and clears the stale building title after preview acceptance. Four new default
tests cover real phase transitions/cache hits, cancellation cleanup, deterministic notification
throttling, and a native HWND title with stale-Generation rejection. The full default suite passes
112 tests, with the existing 40 explicit diagnostics ignored by default. The fullscreen Viewer Release
probe also checks title restoration after a cold build and a warm reopen of a generated PNG.
The Agent did not run an additional private-image build.

### Owner-reported build-progress acceptance: 5/5 passed

On 2026-10-27, the project owner confirmed these results through actual Windows desktop operation
at Head `acea8e8ed37e3b26ee6aa99409537645ccc00919`. They are manual acceptance evidence, separate
from automated tests, not new precise timing measurements or pixel-level proof.

| Check | Owner-reported result |
|---|---|
| Cold first build | Passed: phase, row or Tile progress, and elapsed time display correctly. |
| Build completion | Passed: the title no longer retains a building message. |
| Warm reopen | Passed: existing cache is reused without showing a full build sequence. |
| Switch, cancel, or close during a build | Passed: stale progress does not overwrite the new image title; no hang. |
| Borderless fullscreen | Passed: the hidden title is an accepted design limitation; its state is correct after F11 exits fullscreen. |

Multiple monitors, mixed DPI, negative-coordinate monitors, hot-plug changes, and other unexercised
physical display/DPI combinations remain unverified. Simulated fallback and DPI messages in automated
tests do not complete that coverage. The PR includes fullscreen/restoration/DXGI protection and
large-PNG build progress as separate review topics; no technical blocker requiring a history split
was found. The four implementation/evidence commits are retained, followed by acceptance documentation.

WIC non-preemption/internal allocations, full Device
Lost recovery, real exhaustion and long-term stability remain existing limitations. Stage C first-build
speed/no progressive preview and historical viewport tails remain separate work. Reproduction commands
and retained generated samples are in [development](development.md#d3-a2-fullscreen-regression).
