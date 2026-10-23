[English](stage-d3a2.md) | [简体中文](../zh-CN/stage-d3a2.md)

<!-- doc-id: borderless-fullscreen; revision: 1; status: fullscreen-review -->

# D3-a2: borderless fullscreen and window restoration

This local branch adds F11 borderless fullscreen and Esc restoration to merged D3-a1 geometry.
Automated verification is complete; owner Windows manual acceptance and PR review remain pending.
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
Swap Chain Resize and GIF deadlines. Single-machine results are recorded below, not a baseline comparison.
The environment is Windows, RTX 4080 SUPER, Feature Level 11.1, Rust 1.96.0, Release/locked,
with a 3840×2160 display at DPI 168 (175%). A synthetic DPI-144 message does not establish a real 150% test.

| Metric | Samples | P50 / P95 / maximum (ms) |
|---|---|---|
| UI entry transaction | 40 | 0.494 / 3.449 / 4.240 |
| UI exit transaction | 40 | 0.881 / 3.686 / 3.840 |
| Viewer Swap Chain Resize | 36 | 1.086 / 1.954 / 2.280 |
| Viewer GIF Deadline Lateness | 25 | 5.373 / 14.379 / 15.039 |
| Separate ordinary warm GPU hit → Present return | 110 | 0.235 / 6.594 / 7.095 |

The native probe's process/GDI/USER counts remain 144/2/4 before and after its warmed loop.
The Viewer closes during fullscreen GIF playback and completes cleanup in 22.844 ms (one sample).
Final animation bytes, shared CPU bytes, Credits and queued outputs are zero. Its tiny fixtures report
ordinary CPU/GPU peaks of 672/720 bytes, animation peaks of 192/48 bytes and Credit peak 2;
Tile CPU/GPU peaks are 1,374,840/1,387,380 bytes. Separate Tile regression reaches
63,197,532/93,206,000 bytes within existing limits. These are reported payload categories, not additive
process/VRAM totals; Swap Chain, WIC, driver and OS allocations are not isolated by these counters.
The small workloads and single timing run do not establish stability, latency equivalence or performance improvement.

All durations end at application-side events. Present return is not displayed-frame completion or
Input-to-Photon. There is no new attribution or optimization claim for [Issue #11](https://github.com/pachi-qwq/pixstride/issues/11);
the original 0.394–0.905 ms static GPU-hit P95 difference and DXGI admission fix remain intact.

## Windows manual acceptance: pending

The project owner must run the following desktop checklist. Automated self-owned-window tests do not
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

Every item remains pending owner confirmation. WIC non-preemption/internal allocations, full Device
Lost recovery, real exhaustion and long-term stability remain existing limitations. Stage C first-build
speed/no progressive preview and historical viewport tails remain separate work. Reproduction commands
and retained generated samples are in [development](development.md#d3-a2-fullscreen-regression).
