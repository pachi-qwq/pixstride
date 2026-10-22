[English](architecture.md) | [简体中文](../zh-CN/architecture.md)

<!-- doc-id: architecture; revision: 12; status: animation-playback-review -->

# Architecture and implementation scope

Pixstride targets Windows 10/11 x64 with Rust Stable, Win32/windows-rs, D3D11, and DXGI Flip Model
presentation. Stages A–C are merged. Stage C's static-PNG Tile path passed the owner's six Windows
manual acceptance scenarios. D1-a static GIF posters and the EXIF 5/7 fix are also merged; other
format extensions remain proposals. This page does not reproduce the early internal design drafts.

## Stage A implementation

The UI thread owns the `HWND`, message loop, input, and current view state. A dedicated render thread
creates and owns the D3D11 Device, Immediate Context, DXGI Swap Chain, shaders, and textures. This keeps
D3D11 COM interfaces on one thread and moves device work and `Present` out of the message procedure.
The thread model is not a performance conclusion until interactive latency measurements are available.

```mermaid
flowchart LR
  Input[Win32 input] --> UI[UI state]
  UI --> Snapshot[Latest Render Snapshot]
  Snapshot --> Render[Render thread]
  Render --> GPU[D3D11 and DXGI]
  Render --> Ack[Asynchronous completion]
  Ack --> UI
```

`Arc<Shared>` holds one `Mutex<PendingState>` containing the latest Render Snapshot, a redraw flag, and
a shutdown flag. The UI overwrites this state and signals an auto-reset event; it does not enqueue each
input as a render job. The renderer waits for work and, while a frame is pending, the DXGI frame-latency
handle. When no redraw is pending it waits only for work, so idle rendering does not poll. No COM
interface crosses the thread boundary. The UI keeps pumping messages during resize and shutdown and
does not join the render thread from a window procedure.

The window procedure stores a stable `Box<WindowContext>` pointer in `GWLP_USERDATA` during
`WM_NCCREATE` and clears it during `WM_NCDESTROY`. The UI retains the `HWND` until the render thread
releases its D3D11 resources and signals a UI-owned completion event. The UI waits for messages and
completion with `MsgWaitForMultipleObjects`, joins the worker, and then destroys the window. A panic
is caught at the worker boundary; a completion guard signals the event and falls back to `WM_QUIT` if
signaling fails. Resize
and `ResizeBuffers` run on the render thread; before `ResizeBuffers`, it clears the Immediate Context
and releases the old Back Buffer view. Zero-sized minimized surfaces suspend drawing.

The renderer tries a hardware device at Feature Level 11.1 or 11.0, then tries WARP if device creation
fails. It uses a two-buffer Flip Discard Swap Chain, a frame-latency waitable object with maximum
latency 1, and `Present(1, 0)`. The program creates two 256 × 256 RGBA8 test textures at startup and
uses Left/Right to select their existing Shader Resource Views. `build.rs` compiles HLSL with the
Windows SDK `fxc.exe` and embeds DXBC into the executable.

On a failed `Present`, the renderer reads `GetDeviceRemovedReason`, reports the failure, releases its
resources, and exits. Stage A does not recreate a removed device. Rust COM wrappers provide reference
count management; they do not guarantee apartment, buffer, or lifetime preconditions. No COM object
is shared with the UI thread, and this code does not justify arbitrary `unsafe impl Send`.

| Stage A validation | Current evidence and remaining work |
|---|---|
| Input latency | Owner diagnostics recorded input publication at 3 µs P50 and publication-to-render handling at 168 µs P50, 2,352 µs P99, and 156,976 µs maximum. Samples were not fully classified or controlled; actual display timing remains unmeasured. |
| Thread wakeup cost | Latest-state coalescing is measured by request-origin buckets in the current build. The owner-run aggregate has a 156,976 µs maximum; attribution needs WPR CPU Usage (Precise). There is no UI-thread-rendering comparison. |
| Resize | The owner manually verified repeated resize, minimize, and restore. Zero sizes skip rendering, and the old Render Target is released before `ResizeBuffers`. DPI migration remains untested. |
| Shutdown | `WM_CLOSE` requests asynchronous shutdown; the UI joins the worker after completion before destroying the window. Manual close passed; automated race and Device Lost stress tests remain outstanding. |
| Device Removed | A failed `Present` queries `GetDeviceRemovedReason` and exits safely. Device recreation and injected removal tests are deferred. |
| Resource synchronization | The Immediate Context and Swap Chain stay on the render thread. No image cache or resource reuse pool exists in Stage A. |

## Stage B image path

The UI thread owns the current `Generation` and directory selection. A single bounded MTA worker owns
WIC, the CPU LRU, directory scanning, and adjacent preloads. It sends owned BGRA8 buffers through a
channel; no WIC COM interface crosses threads. The render thread owns the D3D11 texture LRU and performs
texture creation and drawing. A separate bounded identity thread validates source metadata before a
render-thread GPU-cache probe; hits bypass the WIC worker, and misses schedule decode. UI, identity,
decode, and render stages use the same selection Generation to reject obsolete work. The owner approved
same-direction progress during a hold: the UI keeps a lower bound in that existing Generation range,
and the renderer accepts only nondecreasing completed images without changing the target. Release,
reversal, focus loss, and file opening close the interval; the final latest target continues loading.
A newer GPU hit prevents older decoded pixels from replacing it. This adds no input throttling, worker,
or unbounded request queue. Submitted DXGI frames cannot be withdrawn. Raw dimension budgets are
checked before EXIF; the worker retains at most 32 budget rejections by validated Source Revision,
separately from pixel caches and transient read errors. See [Stage B implementation and limits](stage-b.md).

The [D3D11 thread rules](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-intro)
and [Present contract](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-present)
constrain the implementation. Rust COM wrappers manage reference counts; they do not prove apartment,
buffer, or lifetime preconditions. The native common dialog runs on the UI thread; WIC COM objects stay
on the image worker's MTA and never cross to the UI or render thread. No arbitrary `unsafe impl Send`
is justified by this design.

The implementation uses a two-buffer Flip Discard Swap Chain, maximum frame latency 1, and on-demand
rendering. It waits for frame readiness before drawing, including the first frame. Idle rendering
waits for work rather than polling an already-signalled frame handle. These settings follow the
[Flip Model guidance](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)
and [waitable-object contract](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgiswapchain2-getframelatencywaitableobject);
their latency benefit must be measured on an interactive target system.

## Stage C decision: bounded rows and viewport Tiles

Stage C implements a separate static PNG path using `png 0.18.1`, raw PBGRA8 disk Tiles, a ceil-half
pyramid, and a render-thread Tile shader. Ordinary images keep Stage B. The initial 512 MiB Builder
proposal was replaced with a 128 MiB budget after row-buffer experiments; 1024 remains experimental.
Tile CPU/GPU resident budgets are 64/128 MiB, separate from ordinary 512 MiB caches. Bounded replies
retain additional staging buffers. [Stage C](stage-c.md) records the tests, limitations, and measurements.

One lazy background worker builds or reads Tiles; it does not occupy WIC. One latest build request,
one latest viewport request, and two replies bound work. Existing source Generation and Render Snapshot
sequence reject stale work. Cancellation is cooperative; a blocked read can outlive the two-second
shutdown grace but cannot use HWND/GPU/UI handles afterwards. The renderer never reads Tile files.

The preview is only published after full construction. Each visible region draws either its selected
Tile or a corresponding preview region, preserving alpha without overlapping layers. CRC-protected
index/data records and atomic directory publication reject incomplete/corrupt data. Source consistency
uses metadata and three sampled CRCs, not a full or cryptographic identity. An active cache handle
prevents eviction; a root file lease serializes builders and cleanup. No cache database, worker process,
IPC, Job Object, or automatic Device Lost recovery was added.

This choice favors small ownership boundaries and measurable allocation over compressed-cache density
or an early complete preview. Disk write performance, sampled-source false negatives, OS cache growth,
disk exhaustion, driver hangs, and historical viewport-tail attribution remain review risks. The owner
has accepted real desktop gestures; that acceptance does not establish long-duration stability or failure recovery.

## D1-b2 playback boundary

D1-b1 is merged. This branch transfers its unique lazy animation controller from `ImageLoader`
to the Render Thread; WIC COM and sequential composition remain on the single MTA Worker.
The Render Thread owns the playback clock, one future CPU frame and one reusable animation Texture/SRV.
The two output Credits still cover queued, held and in-production frames. No second upload queue exists.

Animation pixels use up to 256 MiB inside the shared 512 MiB CPU ledger. Animation GPU payload,
including Query-retired resources, uses up to 128 MiB inside the shared 512 MiB GPU limit.
Static LRU admission accounts for animation payload and may evict unpinned entries. These are
application payload limits, not hard process-memory or driver-allocation limits. Stage C is unchanged.

Selection Generation, Source Revision, Session ID and Frame Ordinal are checked independently.
The Render Thread waits for work, producer output, a single-shot timer or DXGI frame availability.
The DXGI event is not a GPU completion fence. Pause/finish/static idle remove the timer from the wait set.
UI asynchronously observes Worker completion and joins only after COM/data cleanup; blocked WIC retains
the existing finite shutdown grace and safe detachment boundary. See [D1-b2](stage-d1b2.md) for
policies, measurements, automated evidence and the eight Windows manual scenarios reported passed by the owner.

## Completed stage scope and deferred work

| Stage | Implemented scope or planned candidates | Reserve or defer |
|---|---|---|
| A | Win32 lifecycle, one render owner, latest snapshot, simple sequence/Device Epoch if needed, embedded test texture, QPC events, resize/shutdown/recovery tests. | No file navigation, decoder pool, CPU/GPU image cache, IPC, Job Object, Color Cache, or Tile scheduler. |
| B | Merged WIC JPEG/PNG/BMP decoding, checked pixel layout, source identity, one WIC worker plus one source-validation thread, CPU/GPU LRU caches, directory navigation, adjacent preloads, and stale-generation rejection. GUI acceptance and rapid-navigation retesting passed. | Keep one WIC worker, bounded source validation, and a two-image preload window; defer a general priority framework, ordinary-image panning, and pooled Texture allocator until measured. |
| C | Merged sequential large-PNG streaming, preview, disk Tile Cache/Pyramid, visible-region rendering, zoom/pan, and resource-failure regressions; six owner-run GUI scenarios passed. | Progressive preview and comprehensive Device Lost/resource-exhaustion/stability validation remain open; worker IPC and Job Object isolation require evidence, with no stable plugin or IPC ABI. |
| D (partial) | Merged EXIF 5/7 correction, static GIF posters and background production; branch D1-b2 playback passes automated tests and all eight owner-reported Windows manual scenarios. Code review and PR merge remain pending. | Further formats, basic ICC, configurable input and comic reading require separate approval. Add derived color caches only with a tested color path; HDR remains deferred. |

Stages A–C are complete; further Stage D work requires separate approval. Existing
pixel-layout, cancellation, budget-failure, and decoder-capability boundaries do not authorize extra runtime
services or counters. Each later Generation must
have one owner and a distinct invalidation reason before it is added.

## Ordinary and large images

Ordinary images should use file read → decode → GPU upload → display. For the same validated
Image Key and Device Epoch, a GPU cache hit reuses the Texture without reading the source again,
decoding, scanning the directory, or uploading pixels. File/colour-policy changes are real invalidation
events; weak size/time metadata alone cannot guarantee immutable contents.

Large PNGs need a separate path. Their concatenated IDAT payloads form a continuous compressed
stream; arbitrary rectangles cannot be located by an independent file offset. The Stage C builder
reads rows, creates lower resolutions, and writes independently readable disk Tiles. A complete first
preview may require scanning the entire image stream. See the
[PNG specification](https://www.w3.org/TR/png-3/) and [stress test plan](stress-testing.md).

The early candidate was a 1024-pixel logical Tile with a 512 MiB builder allocation budget.
Stage C compared 512, 1024, and 2048 and now uses 1024 with 128 MiB; see its measured limits. A 640 MiB worker-process limit is a separate proposal, not a measured
working-set bound. Job Object limits provide resource/failure isolation, not a complete security sandbox.
None of these values has been established as optimal.

D3D11 Feature Level 11.x limits a Texture2D side to 16384 pixels. This is a GPU resource limit,
not a file-format or decoder limit, and smaller allocations can still fail.
[D3D11 resource limits](https://learn.microsoft.com/en-us/windows/desktop/direct3d11/overviews-direct3d-11-resources-limits)
support this boundary. Use checked 64-bit size calculations, independent CPU/GPU/disk reservations,
and coarse previews or explicit errors when resources are insufficient. Do not allocate a full huge bitmap
as an unnoticed fallback. Preserve premultiplied-alpha and color-domain semantics across paths.

## Review action register

| ID | Risk or correction | Owner and gate |
|---|---|---|
| A-01 | Render-thread wakeups could worsen tail latency. Keep the model revisable. | Rendering; stage A timing report. |
| A-02 | Resize/shutdown can deadlock with DXGI or retain back-buffer references. | UI/Rendering; stress, debug-layer and lifecycle tests. |
| A-03 | A frame-latency signal is not a GPU resource-completion fence. | Rendering; explicit in-flight/reuse tests. |
| B-01 | WIC may allocate internally or fail to cancel inside one call. | Decoder; per-codec peak/cancellation data and conservative routing. |
| B-02 | Stale completions or file replacement can select the wrong pixels. | Core/Cache; reverse navigation, source changes, and key tests. |
| C-01 | The early 512 MiB proposal was replaced by a 128 MiB Builder budget; estimates still exclude unknown process/allocator/OS-cache overhead. | Large-image path; allocation, process, and OS-cache measurements. |
| C-02 | Tile writes, cache validity, and filtering borders add complexity. | Disk Cache/Rendering; small oracle fixtures before the private sample. |
| C-03 | IPC/Job Object adds startup and recovery cost. | Large-image prototype; justify isolation with cancellation/failure evidence. |
| D-01 | A derived Color Cache may repeat conversions or conflict with OS color management. | Color path; keep deferred until profile/ACM tests exist. |

The original designs and raw environment evidence are retained privately, unchanged. Their detailed
budgets and mechanisms remain proposals. A prior environment probe compiled and created a WIC Factory
and a D3D11 hardware device; it did not test a Swap Chain, viewer, or product latency.
The debug device returned `0x887A002D`; obtain authorization for Graphics Tools installation before
claiming debug-layer validation. That probe supplied no product performance result; subsequent Stage A/B reports are recorded separately.
