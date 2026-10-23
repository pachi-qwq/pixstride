[English](stage-d3a1.md) | [简体中文](../zh-CN/stage-d3a1.md)

<!-- doc-id: view-transform; revision: 1; status: view-transform-review -->

# D3-a1: shared zoom, pan and view geometry

This branch makes ordinary images, GIF posters/animation frames and large-PNG Tiles consume the
same view geometry. Owner-run Windows acceptance and PR review are pending. D1-b2 playback is
already merged; this work changes its geometry, not its clock or decoder.

## Original defect and implementation boundary

At baseline `751023f06d255427614db837fb68f227e80676a4`, `RenderSnapshot` carries `zoom` and `pan`,
but ordinary `Renderer::draw` calls `fit_viewport` with only dimensions and mode. Its centered
Viewport ignores those two fields. GIF playback uses the same ordinary draw path. In contrast,
`large::view::pieces` consumes both fields through its own scale/offset formulas.
Wheel input already retains fractional Delta, but it does not read the cursor position.

`src/view_transform.rs` now holds the pure f64 `ViewTransform`. The ordinary renderer reuses the existing
Tile Quad shader with one 32-byte constant buffer and a full-client D3D11 Viewport. It clips the
image rectangle and matching UVs before converting to f32 NDC, so negative pan or very large
image coordinates never become an invalid D3D11 Viewport. Tile selection uses the same transform,
while retaining its LOD, gutters, fallback, bounded requests and disk format.

## Coordinates and view rules

Source coordinates describe pixel boundaries: `[0, width] × [0, height]`. Pixel centers are at
`(x + 0.5, y + 0.5)`. Client coordinates are physical pixels. EXIF uses the decoded, oriented
dimensions; GIF uses the Logical Screen; Tiles always use the original image dimensions.

| State | Rule |
|---|---|
| Fit base | `min(client_width / image_width, client_height / image_height)`; fits the whole image and may enlarge small images. |
| NativePixels base | 1.0, independent of display DPI. At reset, boundaries are integer-aligned; an odd spare pixel stays on the right/bottom. |
| Effective scale | Base multiplied by extra Zoom, whose unchanged range is 0.01–64. Fit itself is not clamped to that range. |
| Pan | Translation in physical client pixels, applied after scaling. No image-edge clamp; F/1 recenters. A ±10^15 numerical guard prevents unlimited accumulation. |
| Invalid geometry | Zero sizes, non-finite values and invalid zoom return `None`; drawing/Tile selection skips invalid geometry. |
| F / 1 | Select mode, reset extra Zoom/Pan, stop the current drag. |
| New image | Keep mode; reset extra Zoom/Pan when the new image/preview is accepted. During loading, the old image keeps its view. |
| GIF frame/Loop | Keep view, selection Generation and Session; geometry input does not restart playback. |
| Resize | Recompute Fit base and keep the source point under the previous client center under the new center. |
| Minimize | Retain the last nonzero client size. Restoring the same size preserves pan exactly. |

The transform is `p = offset + u × scale`. Wheel zoom first finds `u = (p − offset) / scale`,
then computes `new_offset = p − u × new_scale`. The actual clamped new scale determines Pan;
scrolling farther at a limit does not move the image. The same rule applies over blank space.
Fractional pan after anchored zoom can place pixel boundaries between client pixels; pressing `1`
restores integer alignment and one source pixel per physical client pixel.

Win32 wheel coordinates are signed screen coordinates. `window.rs` sign-extends both components,
uses `ScreenToClient`, and applies `1.2^(Delta/120)` without dropping sub-120 Delta. Left-drag
remains the pan gesture. Capture loss, focus loss, cancellation, DPI migration, F/1, a file switch,
or closing clears the drag origin. See Microsoft's [WM_MOUSEWHEEL](https://learn.microsoft.com/en-us/windows/win32/inputdev/wm-mousewheel)
and [ScreenToClient](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-screentoclient) contracts.

## State and resource ownership

`Shared::activate_view` acknowledges the accepted Generation and final dimensions under the existing
pending-state mutex. It reuses the navigation watermark and progress range, rejects stale candidates,
and returns the latest snapshot to avoid restoring old input after an upload. It transfers only
dimensions; UI math performs no file I/O and never receives COM resources. GIF frame updates do not
call this method. Static GPU cache hits use it without decoding or uploading the image again.

The existing Render Thread still owns every D3D11 object. Geometry updates only change Quad constants;
they do not replace the current SRV or introduce a new worker, timer or render loop. Ordinary-image
limits remain 256 MiB per image and shared CPU/GPU limits remain 512 MiB each. Animation still uses
256 MiB CPU and 128 MiB GPU subquotas within those totals. Stage C budgets and cache format are unchanged.
The fixed 32-byte pipeline buffer is not an image-payload allocation.

## Verification evidence

Automated tests use generated public-safe fixtures and actual Windows WIC/D3D11 resources. They do
not read the private extreme PNG or its cache, and hidden-window tests are not manual acceptance.

| Check | Evidence |
|---|---|
| Pure geometry | Fit/native, oriented non-square sizes, corners, pan, internal/external cursor anchors, fractional Delta, limits, resize and 2,000 zoom round trips. Coordinate tolerance is 10^-7 pixels. |
| Selection state | Same-image acknowledgment keeps view; new accepted/progress images reset it; old Generation cannot reset newer geometry. Minimize/restore and physical-client size changes preserve the observed center. |
| GPU geometry | 7,869 render-target pixels checked against independent bilinear/alpha formulas: six ordinary view cases and ten WIC-composed GIF frames over two passes. Channel tolerance is two RGBA8 units. SRV identity and texture payload remain unchanged. |
| Win32 integration | Negative screen coordinates, Delta 15, left drag and capture/focus cancellation, six ordinary/GIF/Tile selections, GIF Loop/pause/resume, minimize/dialog flags, and 120 transform steps. Those steps add no WIC CopyPixels or image texture upload; a gated 220 ms static-idle observation adds no Present or render wake. |
| Existing paths | All 17 selected Release probes pass, covering EXIF, GIF poster/playback/Delay/Loop/Credit, stale results, cache hits, navigation, synthetic large PNG, Tile seams and shutdown. Tile GPU readback checks another 17,425 pixels. |

The first GPU probe incorrectly interpreted the initial HWND-sized back buffer using the requested
test width. Explicit buffer-size synchronization corrected the probe; the pixel oracle and tolerance
were unchanged. This was a probe setup failure, not evidence of a driver defect.

Long-hold regression also exposed an existing presentation-wait defect on both this branch and the
unchanged baseline. A timeout snapshot showed Generation 31 accepted and uploaded while the last
Present remained at 25–27. If a candidate became stale after consuming a DXGI frame-availability
notification, skipping Present discarded that admission and the next image could wait indefinitely.
The Render Thread now retains one acquired admission until Present. Work notifications remain
higher priority, and idle still waits indefinitely. A deterministic unsignaled-event test and 20
consecutive slow-decode hold runs pass. No timeout was enlarged. This admission is not a GPU fence;
see [GetFrameLatencyWaitableObject](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgiswapchain2-getframelatencywaitableobject).

## Controlled Release measurements

The same machine (RTX 4080 SUPER, Feature Level 11.1, Rust 1.96.0) alternated baseline and branch
processes three times for each workload. Both used `--release --locked`, identical generated
fixtures and the same input sequence. Entries below are ranges of per-run P50/P95, not pooled
percentiles. OS/driver caches and desktop scheduling were not isolated.

| Workload / metric | Samples per run | Baseline P50 / P95 (ms) | Branch P50 / P95 (ms) |
|---|---|---|---|
| Static GPU hit, selection → Present return | 110 | 0.300–0.322 / 5.787–5.917 | 0.279–0.342 / 6.311–6.816 |
| Static CPU hit, selection → Present return | 100 | 0.480–0.491 / 5.380–6.694 | 0.440–0.518 / 5.383–6.714 |
| Partial-frame GIF Deadline Lateness | 60 | 7.401–8.112 / 14.279–14.673 | 7.385–8.208 / 14.241–15.016 |
| Medium GIF Deadline Lateness | 60 | 6.270–7.983 / 14.268–14.803 | 7.107–7.836 / 14.649–14.789 |
| Partial-frame GIF GPU upload submission | 60 | 0.049 / 0.067–0.072 | 0.049–0.053 / 0.063–0.083 |

The static GPU-hit P95 is 0.394–0.905 ms higher in paired runs; three runs do not establish
its cause. Static command preparation P95 rises from 6–8 to 8–10 µs. Cache-hit assertions still confirm
no repeated WIC decode. GIF profiles report no Underflow/failure and retain bounded output/resources.
These results support functional regression coverage, not a latency improvement claim.

The separate 8193 × 4097 Tile probe traverses five positions twice. Its five warm GPU-hit viewports
complete at 113/123 µs P50/P95 without additional Tile reads/uploads. Peak Tile CPU residency is
63,197,532 bytes and GPU payload 93,206,000 bytes, within existing limits. This run includes a cold
build and is not a strict comparison with historical Stage C timings.

## Remaining acceptance and limits

The project owner must run these ten Windows checks before D3-a1 can be described as manually accepted:

1. Zoom/pan ordinary JPEG, PNG and BMP with the wheel and left drag.
2. Keep a chosen detail under the cursor while zooming in/out, including blank-space anchors.
3. Alternate F/1; check 100% pixel mapping at 100%, 150% and 200% DPI.
4. Navigate different dimensions/aspect ratios; retain mode and reset extra view on the accepted image.
5. Zoom/pan a playing GIF; check its Logical Screen remains stable.
6. Pause/resume and let a GIF loop; verify the view does not jump.
7. Use a generated Tile PNG; inspect its four corners, center and seams at Fit/100% and other scales.
8. Resize, minimize/restore, Alt+Tab and move between monitors/DPI settings.
9. Hold/reverse navigation, Ctrl+O/cancel, lose capture during dragging, and close while loading.
10. Leave a static image idle; confirm no sustained abnormal CPU use or periodic refresh.

Real multi-monitor/DPI migration, extreme physical input devices and long-duration stability remain
unverified. Free pan can move an image completely outside the window; F/1 recovers it. WIC cancellation,
Device Lost recovery, resource-exhaustion and Stage C first-build/viewport-tail limits remain as documented.
All timings are application-side; Present return is not screen completion or Input-to-Photon.

`release_view_transform_gpu_pixels` and `release_view_transform_input_and_playback` use
`PIXSTRIDE_TEST_ROOT` for generated inputs. Reproduction commands and fixture locations are in
[development](development.md#d3-a1-view-regression).
