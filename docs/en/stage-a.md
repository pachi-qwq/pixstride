[English](stage-a.md) | [简体中文](../zh-CN/stage-a.md)

<!-- doc-id: stage-a; revision: 9; status: merged -->

# Stage A: Win32 window and D3D11 rendering

Stage A merged through [PR #2](https://github.com/pachi-qwq/pixstride/pull/2). This page preserves its
first runnable slice: a native Win32 window, dedicated D3D11 render thread, and on-demand DXGI presentation.
That slice did not load image files or implement navigation. Those capabilities were added in merged
[Stage B](stage-b.md), followed by large-PNG support in [Stage C](stage-c.md).

## Runtime ownership and event flow

The UI thread creates and owns the `HWND`, handles the Win32 message loop, updates the latest render
state, and signals a Windows auto-reset event. The render thread creates and owns the D3D11 Device,
Immediate Context, Swap Chain, shaders, render target, sampler, and two test textures. No D3D11 COM
interface crosses the thread boundary. The UI keeps the `HWND` alive until the render thread releases
its resources and signals a UI-owned manual-reset completion event. The UI waits for that event and
messages with `MsgWaitForMultipleObjects`, joins the worker, and only then destroys the `HWND`. A
worker guard signals completion even after a caught panic; if signaling fails, it posts `WM_QUIT` as
a fallback. This removes reliance on a window message whose post could fail while the UI waits.

```mermaid
flowchart LR
  Msg[Win32 messages] --> UI[UI thread]
  UI --> State[Mutex: latest snapshot]
  UI --> Event[Auto-reset work event]
  Event --> Render[Render thread]
  Latency[DXGI frame-latency handle] --> Render
  Render --> D3D[D3D11 Immediate Context]
  D3D --> Swap[Flip Discard Swap Chain]
  Render --> Done[Completion event]
  Done --> Join[UI joins worker]
  Join --> UI
```

`Arc<Shared>` shares a small `Mutex<PendingState>` between the two threads. The state contains the
current test texture, client size, sequence number, redraw flag, and shutdown flag. Updates replace
the snapshot instead of appending render jobs, so a burst of navigation input cannot create an
unbounded queue. The auto-reset event wakes the renderer; it is not the state store. While a frame is
pending, the render thread waits for either work or DXGI's frame-latency handle. When no frame is
pending, it waits only for work and does not poll.

The window procedure reads a stable `Box<WindowContext>` pointer from `GWLP_USERDATA`. The UI thread
installs it during `WM_NCCREATE` and clears it in `WM_NCDESTROY`. On close, the UI requests shutdown
and keeps dispatching messages. The render thread clears the Immediate Context, releases the Back
Buffer view and other COM wrappers, then signals the completion event; only after joining the worker
does the UI destroy the window. If `SetEvent` fails, a `WM_QUIT` thread message is the fallback.
`ResizeBuffers` also runs only on the render thread, after clearing context bindings and
dropping the previous Render Target View. A zero-sized minimized client area suspends rendering.

## D3D11 and presentation path

The renderer first requests a hardware D3D11 device at Feature Level 11.1 or 11.0. It tries WARP if
device creation fails. Startup logs the selected driver type, Feature Level, and DXGI adapter name.
The fallback does not retry a later Swap Chain or shader failure. The current code creates a two-buffer `DXGI_SWAP_EFFECT_FLIP_DISCARD` Swap Chain with
`DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`, sets maximum frame latency to 1, waits for the
frame-latency object before drawing, and calls `Present(1, 0)`.

At startup, the program generates two 256 × 256 RGBA8 test images in memory: a checkerboard and a
gradient. It creates one D3D11 Texture2D and Shader Resource View for each. Left and Right select the
existing view; they do not regenerate or upload the pixels. A full-screen triangle samples the
selected texture with a linear sampler. The window stretches the test pattern to its client area.

`build.rs` compiles `shaders/test_pattern.hlsl` with the Windows SDK `fxc.exe` into the Cargo `OUT_DIR`.
The executable embeds the resulting DXBC bytecode; the program does not compile shaders during normal
startup or on a frame. Set `PIXSTRIDE_FXC` to the full `fxc.exe` path when automatic Windows SDK
discovery does not find the compiler.

The renderer checks failed `Present` calls and queries `GetDeviceRemovedReason`. It reports the error
and exits; device recreation and recovery are not implemented in Stage A. Rust COM wrappers release
their references when the renderer is dropped on its owner thread. The UI thread does not own a D3D11
resource.

## Timing and measurement boundaries

The program uses `std::time::Instant`, which is monotonic on Windows and is backed by the system's
high-resolution clock. It stores up to 4096 recent samples per metric in a fixed-capacity ring and
prints count, minimum, maximum, P50, P95, and P99 after shutdown. Percentiles use nearest-rank
selection. It does not synchronously write a log for each frame.

| Metric | Measured interval |
|---|---|
| Startup | Process entry to return from `CreateWindowExW`; not first visible or composed frame. |
| D3D11 initialization | Device, Swap Chain, compiled shader creation, test texture upload, and initial Render Target setup. |
| Device creation | `D3D11CreateDevice`, including a WARP retry if hardware creation fails. |
| Swap Chain creation | DXGI Swap Chain creation and frame-latency configuration. |
| Shader initialization | Vertex and Pixel Shader plus Sampler and Rasterizer State creation. |
| Test texture preparation | CPU test-pixel generation, D3D11 Texture2D creation, and Shader Resource View creation. This measures the CPU/API path; it does not wait for GPU completion. |
| First Present | Render Thread start to the first successful `Present` call return. It includes initialization and the first frame's work. |
| Input handling | Direction-key handling through latest-state publication and event signal. |
| Request to render thread | Snapshot publication timestamp to the render thread consuming that snapshot. |
| Render commands | CPU time for setting D3D11 state, clearing the target, and issuing the triangle draw. |
| Present | Time spent inside `Present(1, 0)`. This is not display time. |
| Resize | CPU time for the render-thread resize and Render Target recreation; zero-size suspension is excluded. |
| Shutdown | UI close request to render-thread exit. It does not include all OS process teardown. |

The Stage A metrics do not prove when a frame reaches the monitor. Use PresentMon/ETW to observe
displayed-frame events, and an external high-speed capture method for Input-to-Photon measurements.
Neither method is part of the current application. Do not interpret a quick `Present` return as a
display-latency result. The First Present metric is an application-side milestone, not a display timestamp.

Owner-run Release GUI acceptance passed: the checkerboard was visible; Left/Right switched textures;
maximize, minimize, restore, repeated resize, and Alt+F4 all worked. The machine used an NVIDIA
GeForce RTX 4080 SUPER with the Hardware Driver at Feature Level 11.1. These are manual functional
checks, not latency or long-duration stability tests.

One owner-run diagnostic session measured D3D11 initialization at 183.236 ms, including 177.742 ms
for device creation. Input-state publication was 3 µs P50; publication-to-render handling was 168 µs
P50, 2,352 µs P99, and 156,976 µs maximum. `Present(1, 0)` call duration was 226 µs P50, Resize
was 1,759 µs P50, and close request to render-thread exit was 14,554 µs. Sample counts and controlled
startup/cache conditions were not recorded, so these are diagnostic observations, not controlled
performance results. Present duration does not measure when pixels reach the display.

The maximum publication-to-render sample cannot be attributed from the aggregate alone. New samples
are bucketed by the latest coalesced request origin: initial size, key input, resize, minimize, restore,
or paint. Coalescing means a bucket labels the snapshot consumed, not every input before it. Separating
scheduler delay from time spent in an in-flight Present/driver call requires a correlated Windows
Performance Recorder CPU Usage (Precise) trace under isolated input and resize workloads. The old
156,976 µs sample cannot be retrospectively classified.

Two local Release launches recorded 2.824 ms and 4.762 ms from process entry to the return of
`CreateWindowExW`, and 223.234 ms and 206.379 ms for D3D11 initialization. These are two diagnostic
samples on one machine, not controlled cold/hot-start measurements or a reliable percentile report.
Both runs logged that the first `Present` call returned successfully. Those runs predate driver and
adapter logging, so they cannot identify the selected driver. Those earlier startup samples are not
controlled cold/hot startup measurements.

## Build and run

Use Windows 10/11 x64 with Rust Stable, the `x86_64-pc-windows-msvc` target, MSVC C++ Build Tools,
the Windows SDK, and `fxc.exe`.

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
cargo run --release
```

The Release executable is `target/release/pixstride.exe`. Closing it prints the accumulated timing
summary to stdout. The Rust CI workflow runs formatting, Clippy, tests, Release build, and Rustdoc on
`windows-latest`. It does not run interactive GUI tests or set the GitHub Ruleset's required checks.

## Verification status

Local Rust validation passed: formatting, Clippy with warnings denied, unit tests, Release build,
and Rustdoc generation. The unit tests cover the bounded timing sample window, percentile calculation,
generated texture data, latest-state coalescing, and terminal shutdown state.

A project-owner manual test on an NVIDIA GeForce RTX 4080 SUPER using the Hardware Driver and Feature
Level 11.1 confirmed visible checkerboard rendering, Left/Right texture switching, maximize/minimize/
restore, repeated resize without a hang or black frame, and Alt+F4 with a timing report. CI verifies
compilation and non-GUI tests, not interactive behavior.

Controlled cold and warm startup statistics, actual Input-to-Photon latency, GPU Device Lost stress,
and long-duration stability testing remain outstanding. Current measurements do not establish that
the thread model meets the 50 ms startup, 5 ms cached-switch, or 1 ms input-processing engineering
targets; those remain targets, not results.
