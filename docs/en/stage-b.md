[English](stage-b.md) | [简体中文](../zh-CN/stage-b.md)

<!-- doc-id: stage-b; revision: 2; status: draft-pr-review -->

# Stage B: image browsing core

Stage B adds an initial implementation for opening JPEG, PNG, and BMP files, browsing the containing
directory, and presenting decoded pixels through the existing D3D11 render thread. The project owner
confirmed that an image displays, zoom controls work, Left/Right navigation changes images, `Ctrl+O`
opens the native dialog, and window resize remains responsive. These checks do not cover every error
path or establish performance percentiles.

## Opening and navigation

Pixstride accepts one optional image path on the command line, including Unicode paths and paths with
spaces. `Ctrl+O` opens the native Windows file dialog. Left and Right move through supported files in
the current directory; navigation stops at either directory boundary. `F` selects Fit to Window and
`1` selects native pixel size. Fit preserves aspect ratio and centers the image. Native pixel mode
centers the image at 1:1 scale and clips portions outside the client area; panning is not implemented.

Directory entries are filtered to regular files with `.jpg`, `.jpeg`, `.png`, or `.bmp` extensions.
Sorting compares Unicode lowercase filenames and then the original filename as a deterministic tie
break. Directory scanning runs on the image worker, not in the Win32 message loop.

## WIC and pixel ownership

One bounded worker thread initializes a COM MTA and creates all WIC objects on that thread. It decodes
frame zero and returns an owned byte buffer; no WIC COM interface crosses a thread boundary. WIC
converts pixels to 32-bit premultiplied BGRA (`GUID_WICPixelFormat32bppPBGRA`). EXIF orientations 1–8
are mapped to WIC flip/rotation transforms when the metadata query is available. The renderer uploads
that layout as `DXGI_FORMAT_B8G8R8A8_UNORM` and uses premultiplied-alpha blending over the clear color.

Dimensions, stride, and allocation length use checked arithmetic before `CopyPixels`. The current
defaults are an individual dimension limit of 16,384 pixels, 256 MiB decoded bytes per image, and
512 MiB each for CPU and GPU resident caches. These are adjustable code defaults, not measured optimal
values. Images exceeding either dimension or the decoded-byte limit are rejected before allocating
the full pixel buffer. The 48,000 × 32,000 stress PNG is outside this path and was not opened.

## Work scheduling and caches

The UI owns one monotonically increasing `generation` for selection changes. The worker keeps one
pending current request, one directory scan, and at most two adjacent preloads. New selections replace
pending work; a WIC call already in progress is allowed to finish, then its result is discarded if its
generation is stale. The CPU LRU is owned by the worker and keyed by canonical path, file length, and
last-write timestamp. This identity detects common replacements but is not a content hash.

The render thread owns the D3D11 texture LRU and the Immediate Context. It reuses an uploaded texture
on a cache hit, charges the cache by width × height × four bytes, and evicts older non-current entries
to its 512 MiB budget. The CPU cache charges the actual pixel-buffer length. A single decode can add up
to 256 MiB alongside the resident CPU cache; temporary disk caching is not part of Stage B.

A current GPU-cache hit still waits for the WIC worker to canonicalize the path, read file metadata,
and check the CPU cache before the renderer can identify the cached texture. A CPU-cache hit avoids
WIC pixel decoding, but not that worker handoff and source-revision check. Keeping this validation off
the UI thread avoids showing a stale texture after a file changes. New selections now discard queued
GPU preloads. The renderer presents the current selection before uploading preloads, then uploads at
most one of the two queued neighbors per wake. A texture upload already in progress cannot be
preempted.

## Diagnostics and limits

On exit, the bounded timing collector reports directory scan, WIC open, converter setup,
`CopyPixels`, total successful decode, texture upload, render-thread request delay, first image
`Present` return, and CPU/GPU cache hit counts with current and peak resident bytes. These byte counts
cover cache-owned entries, not process working memory or driver allocation overhead. WIC owns file
reading, so this version does not isolate storage latency from decoder work. `Present` return is an application-side boundary and does not
measure when photons reach the display.

The current diagnostic build also reports current-selection and preload CPU/GPU hit and miss counts,
source identity plus CPU-cache lookup time, current request-to-worker-result time, and separate GPU-hit
and GPU-miss selection-to-`Present`-return distributions. Hit rates use successful cache lookups as
their denominator; file identity, decode, or upload errors are reported separately and do not count as
cache misses. A GPU hit still requires the worker result described above. The previous owner run
reported aggregate CPU hits/misses
of 26/13 and GPU hits/misses of 26/5, including preloads; this corresponds to 66.7% and 83.9% aggregate
hit rates for that run, not navigation-only rates. Its 11 image-request-to-render samples had a 202 µs
P50 and a 161.58 ms maximum. The sample mixed request origins and included a long-tail event, so it is
not a controlled cache-hit latency benchmark. Re-run the new counters while repeatedly navigating a
small local directory to get selection-only rates and percentiles.

The automated suite includes path sorting and boundaries, checked image layout and budgets, EXIF
mapping, CPU cache eviction, stale-generation replacement, cancellation of queued preloads, viewport
sizing, rejection of truncated JPEG data, a generated BMP decoded through WIC, and generated JPEG/PNG
samples encoded and decoded through WIC. The owner has verified basic real-image presentation, zoom,
navigation, dialog opening, and resize. Slow or cloud-backed file reads, rapid reverse navigation during a long decode, and closing
while a WIC call is still running need additional manual testing. WIC calls are synchronous and cannot
be cancelled once started. On shutdown, Pixstride requests cancellation and waits up to 2 seconds; if
the worker remains in WIC, it detaches the thread and exits the process so Windows can reclaim its
resources. This bounds application shutdown, but it does not cancel WIC safely inside the running
process. The owner reproduced `The cloud file provider is not running.`
when a OneDrive-backed JPEG was unavailable locally. Pixstride did not alter that file; cloud-provider
availability is outside the decoder's control. Device Lost recovery, long-duration stability, panning,
GIF, TIFF, WebP, RAW, AVIF, HEIC, JPEG XL, large-image tiles, and disk caches remain unverified or
outside this stage.

Build on Windows 10/11 x64 with the documented MSVC and Windows SDK toolchain:

```powershell
cargo build --release --locked
cargo run --release -- "<path-to-image>"
```

Without a path, the program starts with the Stage A generated test texture. Run `cargo run --release`
and use `Ctrl+O` to select an image. The title bar shows the selected path. See [development](development.md)
for checks and the current validation status.
