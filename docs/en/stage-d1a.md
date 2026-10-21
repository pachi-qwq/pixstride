[English](stage-d1a.md) | [简体中文](../zh-CN/stage-d1a.md)

<!-- doc-id: gif-poster; revision: 4; status: pr-review -->

# D1-a: GIF first-frame posters

This branch implements a static poster for basic GIF87a/GIF89a files. A poster is the first image
frame composed at its offset on the Logical Screen. Later frames do not alter it. Animation,
playback clocks, Loop policy, pause controls and animation cache reservations are outside this change.
The EXIF 5/7 fix was merged through [PR #6](https://github.com/pachi-qwq/pixstride/pull/6)
and is included in this branch.

## Decode and cache contract

The existing background WIC Worker validates the GIF structure before opening WIC. It checks the
signature, screen, palettes, frame rectangles, extensions, sub-block lengths, terminators and Trailer.
The scan uses an 8 KiB reader buffer and fixed-size metadata storage. It checks cancellation between
reads and blocks. It does not implement LZW or guarantee detection of damaged pixel codes.

Trailer remains mandatory. After it, the scan accepts only zero padding within the unchanged
64 MiB total file/scan limit. A fixed 8 KiB buffer checks every chunk and cancellation between
reads; short reads do not imply end-of-file. Nonzero trailing data is rejected. This compatibility
rule does not relax palette, extension, Sub-block or terminator checks.

WIC supplies the first local frame and PBGRA8 pixels. The decoder checks WIC screen/rectangle metadata
against the scan, allocates one zeroed Logical Screen canvas and copies the local pixels into it.
A full-screen frame uses one CopyPixels call. An offset frame copies rows directly into that canvas,
avoiding a second application-owned full-frame buffer. Transparency outside the frame is retained.
Disposal does not apply after this static poster, because no following frame is composited.

The default background policy is transparent even when a valid global background palette entry exists.
The opaque pixels inside the frame retain their palette color; transparent indices remain transparent.
Deterministic tests include a green global background, an opaque black local pixel, local palettes,
and a file without a global table. No other-viewer comparison has been performed; this is an explicit
policy, not a claim that all viewers render the same background.

SourceIdentity now also records ImageContent::GifPosterV1 for this fixed output policy. CPU/GPU
cache keys include it, source path, length and mtime. The source is rechecked before publication.
Common replacements invalidate the entry; same-metadata in-place rewrites remain a known limitation.
A read-only handle denies new write/delete handles throughout preflight and WIC decoding.

The existing ImageLoader, GPU fast path, preloading, Generation and same-direction hold progress
remain in use. A GPU hit does not enter WIC. A CPU hit skips preflight and decoding and uploads only
when the GPU entry is absent. The render thread owns the texture and Immediate Context. Errors use
the existing title/stderr status so another file can still be selected.

## Admission and resources

| Resource | Limit or behavior |
|---|---|
| Decoded Logical Screen | 256 MiB; each dimension at most 16,384 |
| CPU / GPU ordinary caches | 512 MiB each, shared with ordinary images |
| GIF file scan | 64 MiB; complete structure must fit this limit |
| Metadata / frames / blocks | 4 MiB / 10,000 / 1,000,000 |
| Application pixel storage | One Logical Screen canvas; no separate full patch or frame list |

These are admission limits, not GIF format limits. Plain Text Extension, nonzero data after Trailer,
unknown block markers, invalid reserved fields and unsupported metadata are rejected. Comments and
Application Extensions are bounded and skipped; their Loop content does not establish playback.
GIF content disguised under another supported extension is rejected to prevent bypassing preflight.

The application cannot enforce a hard limit on WIC internal allocations. A blocked file read or WIC
call cannot be safely preempted; cancellation takes effect at the next checked boundary. The UI and
render threads remain separate. Large GIFs do not route into the PNG Tile Builder. No global budget
was increased, and the D1-b CPU/GPU animation sub-budgets and Credits were not added here.

## Verification and measured scope

Production decode tests compare all pixels against hand-authored coordinates, including multi-frame
input, local palettes, transparency, GIF87a, missing global palettes and common source revisions.
They reject every truncated prefix of a small fixture, the Prototype's truncated pixel example,
invalid metadata, scan/metadata limits and over-budget screens. Cancellation and read-only locking
are tested. No private image or large-PNG cache is used.

Padding regressions cover no padding, 167 zero bytes, multiple buffer-sized chunks, short reads,
nonzero/mixed trailing data, missing Trailer, truncated Sub-blocks, cancellation and the exact
64 MiB boundary. Production WIC tests compare padded and unpadded pixels and verify source revision
invalidation. GPU readback and Viewer diagnostics also use a synthetic 167-byte-padded GIF.

Explicit Windows Release diagnostics cover actual GPU readback, SRV reuse, CPU hit without repeated
preflight, GIF/JPEG/PNG/BMP navigation, stale completion after a newer GPU hit, revisions, error
recovery, Fit/100%, Resize, maximize/minimize/restore, and shutdown with a pending GIF task.
The hold diagnostic injects 80 ms of Worker delay; it tests progress/reversal/release semantics,
not real GIF decoding speed. These are automated window/API checks, not manual visual acceptance.

One local Release run on Windows x64 decoded two generated 2048×1024 inputs 32 times each without
clearing the OS file cache. Whole-screen decode P50/P95/P99 was 10.867/11.630/17.052 ms; offset-frame
decode was 9.937/11.520/11.650 ms. Each output owned 8 MiB of canvas pixels. Whole-process Working Set
peaked at 20,209,664 bytes; the largest sampled PrivateUsage was 10,498,048 bytes. These values include
WIC, DLLs, allocator and test-harness overhead, not a WIC-only allocation estimate or a universal bound.
The GPU probe retained 8,388,656 bytes of texture payload (8 MiB plus a 48-byte poster), reusing SRVs.
Payload accounting excludes driver allocation granularity and other GPU resources.

The whole-screen input contains two image frames and ran first; the offset input contains one frame.
The first run also includes initial WIC codec setup. These are separate workloads, not a controlled
comparison of whole-frame and row-copy strategies.

Present return, decode completion and application-side timing do not measure Input-to-Photon.
The existing Stage C synthetic integration diagnostic is run separately with a dedicated cache;
private extreme PNG testing and long-duration stability are outside this task.
Compatibility with all real-world GIF encoding variants has not been verified. Broad long-duration
stability and Device Lost testing also remain incomplete.

## Windows manual acceptance

On 2026-10-21, the project maintainer reported that all five checks below passed on a real Windows
desktop using the Release build with the zero-padding fix, code commit
`ad1df3692d449dba87c0fa93baecd88d5f4f0172`. These are maintainer-provided observations, separate from
the automated window/API diagnostics above.

| Check | Result |
|---|---|
| Transparent GIF poster and empty transparent areas | Passed |
| Local Frame Rect alignment; the poster remains static | Passed |
| GIF/JPG/PNG/BMP round trips, held arrows and reverse navigation | Passed |
| Fit/100%, Resize, Alt+Tab, Ctrl+O and Alt+F4 | Passed |
| Malformed GIFs, zero-padded GIFs and continued browsing after an error | Passed |

This acceptance covers static posters only. GIF animation playback is not implemented, and the
admission, background, WIC and measurement limits described above still apply.

## Build and repeat manual checks

Set PIXSTRIDE_TEST_ROOT to a dedicated writable directory outside the checkout, then run:

```powershell
cargo build --release --locked
cargo test --release --locked release_prepare_gif_poster_samples -- --ignored --nocapture
& .\target\release\pixstride.exe (Join-Path $env:PIXSTRIDE_TEST_ROOT 'gif-poster-gui\partial-transparent.gif')
```

Use this checklist for desktop regression checks and additional shutdown validation:

1. Verify the partial transparent poster uses the viewer background outside its rectangle, rather
   than the file's green palette background. Compare partial-opaque.gif and confirm the local black pixel.
2. Navigate to the generated JPEG/PNG/BMP files and back, rapidly hold/reverse/release arrows, and
   confirm the latest final selection appears. The GIF must remain static.
3. Use Ctrl+O, F (Fit), 1 (100%), Resize, maximize/minimize/restore and Alt+Tab.
4. Open broken.gif, continue to a valid file, and close with Alt+F4. Repeat opening and closing while
   loading memory-full.gif; check for a residual process.
5. Compare padded-transparent.gif with partial-transparent.gif; their posters must match. Retry
   locally available GIFs previously rejected for zero padding. They must still remain static;
   animation playback has not been implemented. Record any remaining compatibility failures.

Implementation references: [GIF parser/compositor](../../src/image/gif.rs),
[pixel fixtures](../../src/image/gif_tests.rs), [Viewer diagnostic](../../src/app/gif_tests.rs).
Block semantics follow [GIF89a](https://www.w3.org/Graphics/GIF/spec-gif89a.txt);
WIC metadata uses the [documented query paths](https://learn.microsoft.com/en-us/windows/win32/wic/-wic-native-image-format-metadata-queries).
