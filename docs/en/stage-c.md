[English](stage-c.md) | [简体中文](../zh-CN/stage-c.md)

<!-- doc-id: stage-c; revision: 10; status: merged -->

# Stage C: large PNG path

Stages A–C are merged. [Stage C PR #4](https://github.com/pachi-qwq/pixstride/pull/4) was squashed as
`3a1b6b251bf70a07d5c9906bcfd3da13980b18c8`. Its main
[Rust](https://github.com/pachi-qwq/pixstride/actions/runs/39442403113) and
[Documentation](https://github.com/pachi-qwq/pixstride/actions/runs/39442402998) CI passed.
The private 48000 × 32000 PNG passed full cache construction and programmatic D3D11 viewport tests. The owner also
confirmed normal cache-hit operation on the Windows desktop and subsequently reported all six final
manual acceptance scenarios passed. Remaining performance and stress limits are recorded below.

## First-build feedback on the D3-a2 branch

The D3-a2 feature branch adds progress to the window title and launch console. It reports source/cache
checks, disk reservation, scanned rows and pyramid generation, Tile checksums, disk synchronization,
index publication, and preview preparation. Row and Tile percentages describe their own phase;
100% of rows does not mean that the cache is ready. Elapsed time is a snapshot at each update, not an
ETA. A blocked system I/O call can leave the last phase unchanged.

Updates use one latest-value slot separate from cache-probe/error replies. Within a phase, notifications
are limited to one per second; phase changes and phase completion notify immediately. Only the current
file Generation can update the title. The Renderer restores the file title after accepting the uploaded
preview, fixing a stale “building” title after completion. Errors keep the existing non-modal handling.
There is no periodic idle timer or redraw from progress. In fullscreen, use F11 to see the title or
Alt+Tab to the launch console; this change does not add an in-image overlay.

This follow-up does not change the cache format, budgets, build algorithm, or progressive-preview
status. Tests use generated small PNGs to cover phase order, throttling, cache-hit scan avoidance,
cancellation cleanup, and stale notifications. Private images and existing private caches are untouched.
The earlier Stage C evidence below is unchanged.

### Owner-reported Windows acceptance of build progress

On 2026-10-27, the project owner confirmed that all five checks below passed on a real Windows desktop
using verified Head `acea8e8ed37e3b26ee6aa99409537645ccc00919`. These are owner-provided manual results,
separate from automated tests; they do not add precise performance measurements or pixel-level proof.

| Check | Owner-reported result |
|---|---|
| Cold first build | Passed: phase, row or Tile progress, and elapsed time display correctly. |
| Build completion | Passed: the title no longer retains a building message. |
| Warm reopen | Passed: existing cache is reused without showing a full build sequence. |
| Switch, cancel, or close during a build | Passed: stale progress does not overwrite the new image title; no hang. |
| Borderless fullscreen | Passed: the hidden title is an accepted design limitation; its state is correct after F11 exits fullscreen. |

This acceptance does not establish coverage of multiple monitors, mixed DPI, negative-coordinate
monitors, or hot-plug changes; those physical scenarios remain unverified. First-build cost, lack of
progressive preview, blocking system I/O, historical viewport tails, [Issue #11](https://github.com/pachi-qwq/pixstride/issues/11),
Device Lost recovery, exhaustion/stability gaps, and the existing display-timing limits remain open.
Present return is not actual displayed-frame completion. The feature branch still awaits PR review
and merge; this acceptance does not change the original Stage C performance evidence.

## C1 row prototype

`large::stream` reads non-interlaced static PNG rows using `png 0.18.1`. It expands RGB, palette, and
grayscale inputs to 8-bit channels, converts each borrowed row to premultiplied BGRA8, and does not
allocate a complete bitmap. Adam7, APNG, 16-bit channels, ICC, and HDR are outside this initial path.
The CPU conversion rounds encoded channel values; it does not perform linear-light color processing.

The decoder has a 64 MiB best-effort limit, with a separate 4 MiB row limit. Chunk-header preflight
uses checked 64-bit offsets, caps ancillary chunks at 1 MiB each and 8 MiB combined, and caps chunk
count. PNG decoding verifies CRC and Adler checksums; ignored text/ICC payloads are not interpreted.
Cancellation is checked between header chunks and rows; a blocked filesystem read cannot be preempted.
These limits are not a proof of a hard process allocation bound.

Small generated RGBA rows matched WIC within one channel value of premultiplication rounding.
CRC corruption, truncation, oversized metadata, cancellation, and arithmetic limits passed tests.
An explicit Release probe ran separate processes for generated 8192 × 256 and 8192 × 8192 PNGs:
owned input/output row capacity was 65,536 bytes in both, and peak working set was 10,022,912 bytes
in both. Scan durations were 13.698 ms and 422.538 ms. This tests a 32× height change, not every PNG
encoding or malformed-input allocation pattern. At that C1 checkpoint, the private extreme image had not been read.

## C2 disk pyramid

One row scan builds all ceil-half levels down to 1 × 1. A 2 × 2 box filter averages premultiplied
encoded channels and repeats odd edges. It retains rows, not complete levels. Tiles have a one-pixel
neighbor gutter; tests compare every level against an independent small-image reference and verify
all level-zero gutters, including partial edge tiles.

The experimental default is 1024. A generated 2049 × 1537 RGBA Release run produced:

| Edge | Tiles | Data bytes | Build ms | Warm reopen ms | Reverse tile reads ms |
| --- | --- | --- | --- | --- | --- |
| 512 | 38 | 16,986,000 | 386.591 | 0.328 | 4.173 |
| 1024 | 19 | 16,910,888 | 436.699 | 0.342 | 3.955 |
| 2048 | 14 | 16,875,968 | 513.083 | 0.311 | 3.738 |

These are single warm-filesystem samples, not percentiles or GPU upload measurements. Conservative
owned-buffer estimates were 1,189,994 / 4,343,154 / 12,752,050 bytes, excluding decoder allocation.
For the 1024 sample tile, PNG reduced 4,210,704 bytes to 1,192,620 but added 135.821 ms encoding and
4.321 ms decoding. The initial cache stores raw PBGRA8, trading disk space for simpler bounded reads.

`pixels.bin` contains padded tiles. `index.bin` records format version, dimensions, source identity,
levels implied by checked geometry, 64-bit offsets/lengths, and tile CRC32; the index has its own CRC.
Readers validate lengths and geometry before allocation and validate each tile when read. The default
aggregate disk budget is 2 GiB, Builder budget 128 MiB including the decoder allowance, with 1 GiB
additional free-space headroom. Neither estimate is a hard process-memory limit.

A root file lease serializes builders, opens, and cleanup. Active read handles prevent eviction.
Completed data/index files are synced and published by a same-volume directory rename; cancellation
removes the partial directory. A later build cleans recognized abandoned partial directories. Invalid
metadata is rebuilt; tile CRC failures report an error and require cache cleanup before rebuilding.
Published caches are evicted oldest-first when a build needs space; this is not an LRU database.
Another process building the same root returns a busy error rather than waiting without a bound.

Source validation compares canonical path, length, modification time, and CRC32 samples from the
first/middle/last 64 KiB. A read guard denies source writes/deletes during construction and the stamp
is rechecked before publication. Changes outside the sampled bytes with preserved metadata can evade
this check; CRC32 is not a cryptographic content identity. Cache-version, sampled-content-change,
active-cache pinning, corruption, overflow, and interrupted-build tests passed.

At the C2 checkpoint, the private source had only had its file attributes checked. The owner approved a separate 10 GiB
cache experiment after C3 passes; its estimated 1024 pyramid is 8,224,561,220 bytes. No source pixels
have been read at this checkpoint.

```powershell
cargo test --release --locked release_tile_sizes_probe -- --ignored --nocapture
```

## C3 viewer integration

Over-budget PNGs route to the row builder before WIC pixel allocation. Ordinary JPEG/PNG/BMP retain
WIC, CPU/GPU caches, and Generation. Uncached PNGs add a small header read; cloud placeholder attributes
are checked first, and those PNGs require manual download. Large JPEG/BMP and unsupported PNG encodings
still report errors. Ordinary budgets remain 256 MiB per image and 512 MiB per CPU/GPU cache.

A lazily started Tile worker owns construction and reads: one latest build slot, one latest viewport
slot, at most two replies, and a 64 MiB CPU Tile LRU. Replies, active upload, and preview retain extra
bounded Tile buffers. The render thread owns a separate 128 MiB Tile GPU cache. At most 24 visible
Tiles are requested; unusually large viewports use a coarser layer. The Builder's 128 MiB budget
includes its decoder allowance. OS file cache and allocator overhead are separate. No IPC is introduced.

Selection Generation invalidates source work; the existing Render Snapshot sequence invalidates
viewport work. Cancellation checks run between rows/Tiles. A blocked read cannot be preempted:
shutdown waits up to two seconds, then detaches a worker with no HWND, GPU interfaces, or usable UI-event
access. A later build cleans recognized partial caches. Device failure keeps error-and-exit behavior;
device recreation and actual Device Lost stress remain unverified. Ordinary Tile allocation failures
keep an available coarse preview and report a non-modal error; forced GPU allocation failure was not
injected. A panicked Tile worker rejects subsequent builds with a restart instruction.

The renderer draws each visible logical region once, using the Tile if ready or its preview UVs if
missing. It never blends a Tile over the same preview pixels. One-pixel gutters prevent boundary
sampling across unrelated data. The first complete preview currently waits for the full PNG scan,
pyramid writes, checksum pass, and atomic publication. There is no partial-file resume or early
complete-preview promise.

Press **F** for Fit, **1** for native pixels. On the large-image path, wheel zooms around the window
center and left-drag pans. Resize updates visible Tiles; minimize suspends drawing. The worker sleeps
on a condition variable; rendering retains event-driven waits.

A Release HWND/D3D11 probe used generated 20000 × 65 PNGs: build 122.490 ms, warm metadata reopen
0.331 ms, selection-to-preview Present return 123.681 ms first / 1.886 ms cached. Tile read/CPU lookup
P50 was 61 µs (11 samples), upload P50 131 µs (13 samples), Tile GPU peak 1,724,772 bytes. These are
synthetic samples, insufficient for stable tail percentiles. GPU readback checked 17,425 pixels across
a Tile boundary, including premultiplied-alpha composition, within two channel values. Fit/native,
pan, zoom, minimize/restore, reopen, ordinary GPU-hit navigation during construction, and close during
construction ran. This does not establish visual desktop acceptance or input-to-photon latency.

```powershell
cargo build --release --locked
$env:PIXSTRIDE_TILE_CACHE = Join-Path $env:PIXSTRIDE_TEST_ROOT 'viewer-cache'
& .\target\release\pixstride.exe $env:PIXSTRIDE_STRESS_IMAGE
cargo test --release --locked release_large_viewer_probe -- --ignored --nocapture
cargo test --release --locked release_tile_gpu_readback -- --ignored --nocapture
```

Set both path variables outside Git. Default cache: `%LOCALAPPDATA%/Pixstride/tiles-v1`. Use a dedicated
root, never a source folder. `PIXSTRIDE_TILE_CACHE_GIB` accepts 1–10, defaults to 2; values above 2 require
separate owner approval. The real experiment has approval for 10 GiB after C3 passes; the C4 results
below record its execution. The owner's final six-scenario manual acceptance is recorded below;
it does not replace the remaining stress tests or extend format/color support.

## C4 real-sample results

After C3 passed, the owner authorized a dedicated 10 GiB local cache. Residency flags and free space
were rechecked before reading. The source was opened read-only, never copied or uploaded, and its
length and modification time were unchanged afterwards. It is a non-interlaced RGBA8 PNG, 48000 × 32000,
2,071,102,933 source bytes; a full RGBA8 bitmap would require 6,144,000,000 bytes.

| Measurement | Result and boundary |
| --- | --- |
| First full build | 755.866 s; row scan plus pyramid writes 750.860 s, followed by CRC/sync/publication |
| Persistent cache | 2,027 Tiles; 8,224,561,220 data bytes + 81,228 index bytes + 8-byte ownership marker |
| Builder process | Peak working set 16,023,552 bytes; sampled private bytes peak 7,577,600 |
| Source row buffers | 384,000 bytes total input/output capacity; no complete bitmap |
| Warm metadata reopen | First run: n=20, P50 0.313 ms, P95 0.470 ms; separate verification: P50 0.408 ms, P95 0.548 ms |
| Corner/center Tile reads | Five level-zero Tiles, 0.123–1.044 ms including CRC, warm filesystem |
| Warm Viewer preview | 170.994 ms from request to preview Present return, n=1, includes a new D3D11 device |
| Viewport completion | n=53, P50 6.993 ms, P95 9.815 ms, maximum 133.217 ms; includes initial device/startup wait and mixed GPU reuse/misses |
| Tile disk read + CRC | n=166, P50 0.957 ms, P95 1.071 ms; no decompression in raw cache |
| Tile GPU upload submission | n=167, P50 0.332 ms, P95 0.419 ms |
| Viewer shutdown | 26.417 ms for render/loader join and window destruction, n=1 |

The RTX 4080 SUPER hardware device reported Feature Level 11.1. Two 5 × 5 native-pixel viewport passes
exercised eviction: CPU Tile resident peak 66,845,952 bytes of 67,108,864; GPU Tile logical peak
134,151,296 of 134,217,728 bytes. DXGI process-local video usage sampled peak was 174,297,088 bytes,
including swap-chain and driver/other allocations. Viewer peak working set was 145,068,032 bytes;
sampled private commitment peak was 421,969,920. These are distinct from the application cache budgets.
OS filesystem cache usage was not measured, and GPU usage samples are not a continuous residency trace.

The Viewer probe loaded only the authorized source through the large-image request, skipping private
directory scanning and adjacent files. The real first build ran in the independent cache probe; its
full selection-to-preview timing and twelve-minute Viewer responsiveness were not measured. It submitted a preview and native regions, returned to Fit,
verified a 250 ms idle interval without additional Present, and closed normally. It does not establish
visual seams/gesture quality on a human-operated desktop. Real build interruption was tested on
generated sources, not by interrupting the private 12-minute build.

This first build is slow. More than a million small write calls were observed while it ran; row-write
batching is a follow-up experiment. There was no controlled storage/decoder comparison, so this is not
a claim that PNG decoding alone caused the wall time. No cache was rebuilt merely to repeat the timing.
The working-set measurements demonstrate this input's streaming behavior, not a hard process-memory
ceiling for every decoder/driver. The small C1 repeat also passed its limits: peak working sets
10,141,696 / 10,158,080 bytes for heights 256 / 8192, with 65,536 owned row bytes.

Ordinary navigation regression passed with 110 GPU hits, 100 forced CPU-hit paths, source replacement,
read errors, reverse/release/focus-loss/new-open boundaries, and a busy WIC worker. GPU-hit P50/P95 was
0.241/6.457 ms; CPU-hit 0.454/5.097 ms. A synthetic hold run produced 25 image submissions, 14 progress
images, and final target/Present Generation 61. These runs overlapped the separate cache-build process
and are not controlled Stage B before/after comparisons. JPEG/PNG/BMP correctness tests also passed.

A subsequent alternating baseline comparison ran the same generated-BMP Release navigation probe
three times per version, with no large-image builder active. Stage B used commit `0e39b64`; Stage C
used the pre-merge test build at that checkpoint, on the same host/toolchain. Each run contained 110 GPU-hit and 100 forced
CPU-hit selections, plus failure/stale-result assertions. All six runs passed:

| Round | Version | GPU P50 µs | GPU P95 µs | CPU P50 µs | CPU P95 µs |
| --- | --- | --- | --- | --- | --- |
| 1 | stage-b | 271 | 6523 | 435 | 3329 |
| 1 | stage-c | 259 | 5894 | 445 | 3489 |
| 2 | stage-b | 264 | 6590 | 427 | 2209 |
| 2 | stage-c | 273 | 6743 | 420 | 4191 |
| 3 | stage-b | 280 | 6701 | 445 | 3999 |
| 3 | stage-c | 275 | 6579 | 434 | 3699 |

Median processing paths stayed in the same sub-millisecond range; CPU tails varied. This short synthetic
probe does not prove unchanged real-directory tail latency or constitute a controlled startup test.

### Owner's desktop cache-hit run

The owner ran the Release viewer on the same real source and reported normal operation. This was a
cache-hit run on the RTX 4080 SUPER Hardware device, Feature Level 11.1; it did not repeat construction.
The following values come from that supplied shutdown report, independently of the scripted probes:

| Measurement | Result and boundary |
| --- | --- |
| Cache open | 10.008 ms, n=1, completed cache with 2,027 Tiles |
| Selection to preview Present return | 203.761 ms, n=1; D3D11 initialization was 181.830 ms |
| Viewport completion | n=798; P50 0.651 ms, P95 2.947 ms, P99 145.561 ms, maximum 426.249 ms |
| Disk Tile read + CRC | n=251; P50 17.067 ms, P95 42.768 ms, maximum 59.585 ms |
| GPU Tile upload submission | n=250; P50 0.363 ms, P95 1.809 ms |
| Tile resident peaks | CPU 66,441,600 / 67,108,864 bytes; GPU 134,140,928 / 134,217,728 bytes |
| DXGI process-local usage | Sampled peak 229,634,048 bytes, including non-Tile allocations |
| Shutdown | 23.977 ms, n=1 |

Target and last successful Present Generation both ended at 1. Two adjacent-file preloads failed with
`0x8007016A` (cloud file provider not running); the current image had no load-failure notification.
The ordinary-worker trace labels the large-image routing rejection as `error`, although the Tile path
then succeeds. This diagnostic label does not indicate a failure to display the current image.

At this checkpoint, the report confirmed normal cache-hit operation without certifying every seam,
gesture, cancellation, or repeated-open scenario. The later final acceptance is recorded separately.
The viewport tail remains a follow-up item. Filesystem cache state and the
interaction sequence were not controlled, so these figures are not a strict comparison with the
scripted warm-cache run. High request/Present counts alone do not establish idle rendering activity.

All timings above stop at application/API boundaries. They do not measure displayed-frame completion
or input-to-photon latency. First build n=1 and the small reopen/viewport sample counts cannot establish
stable P95/P99. Controlled cold/warm startup, Device Lost injection/recovery, disk exhaustion, forced
allocation failure, driver-stalled shutdown, and long-duration stability remain
unverified. EXIF Orientation, ICC/HDR, APNG, Adam7, and 16-bit large-PNG handling are outside this path;
large PNG orientation metadata is not applied.

The initial C4 checkpoint had 58 passing Rust tests and 10 explicit diagnostics omitted by default; document
tool regression has 41 passing tests. Formatting, Clippy, Release build, Rustdoc, document checks and
diff whitespace checks passed. These were local checkpoint results; merged-head CI is recorded above.

For an approved real-cache probe only:

```powershell
$env:PIXSTRIDE_APPROVED_LARGE_CACHE = '1'
$env:PIXSTRIDE_TILE_CACHE_GIB = '10'
cargo test --release --locked real_large_cache_probe -- --ignored --nocapture
cargo test --release --locked real_large_viewer_probe -- --ignored --nocapture
```

Set the source/cache environment paths first. The approval variable records an external decision;
setting it does not grant permission. Keep the cache root available to reuse the completed pyramid.
Close all viewer/probe instances before deleting that dedicated root; the next open will need a full
build again. Partial directories are cleaned on a later build; completed caches do not resume partial
work and corrupted Tile data requires explicit cleanup.

## Performance review with synthetic sources

This review uses Release builds on the same Windows host and local volume. It never opens or rebuilds
the private source. Generated 1025 × 769 and 4097 × 2049 RGBA PNGs each ran three alternating file-output
and discard-output trials. The discard trial executes the same decode, conversion, gutters, and
pyramid code, but its writer discards bytes. No OS cache purge was attempted. Three trials identify
costs, not stable P95/P99 or a prediction of the original 755.866 s build.

| Stage | 1025 × 769 median ms | 4097 × 2049 median ms |
| --- | --- | --- |
| Complete file build | 115.425 | 1094.517 |
| Scan with file writes, nested in build | 24.549 | 817.684 |
| PNG row reads/decode, nested in scan | 2.994 | 31.626 |
| PBGRA row conversion, nested in scan | 2.311 | 25.363 |
| Seek/write calls, nested in pyramid visitor | 16.195 | 732.448 |
| Tile reread and CRC after scan | 1.167 | 11.235 |
| Data `sync_all` | 55.202 | 214.585 |
| Index write and `sync_all` | 24.214 | 27.969 |
| Scan with discard output | 8.864 | 84.989 |

The medium case writes 45,055,512 bytes through 14,936 row calls. Its visitor minus measured writes
costs roughly 26–28 ms, covering gutters, downsampling, allocation, and callback overhead. Row writes
and synchronization dominate this synthetic workload. The original run lacked these stage counters;
its 750.860 s scan included both PNG work and pyramid writes. Its exact attribution remains unknown.

A separate 32 MiB sequential-write control changed only block size and alternated order. Three runs
used 8,192 calls at 4 KiB versus 128 calls at 256 KiB. Median write time was 34.091 versus 8.961 ms;
median final sync was 231.978 versus 174.083 ms. This tests call granularity, not the pyramid's
interleaved offsets. It supports a follow-up experiment with bounded multi-row batches, not a claimed
end-to-end speedup. Such buffers must be charged to the existing 128 MiB Builder budget, with a smaller
batch fallback. Sync-before-publication remains required; it was not removed to improve the numbers.

`CacheReport` now separates file reservation, stream row decode, conversion, visitor, row writes,
post-scan checksum, data sync, index sync, and publication. Per-row clocks add diagnostic overhead.
Timing scopes are nested and cannot all be added together.

### Viewport tail diagnosis

The reported 426.249 ms real-image maximum has no historical per-request stage trace. This review
cannot assign it to disk, CRC, or scheduling retrospectively. A completed viewport may require up to
24 serialized worker reads, and its end event includes uploads and Present. Summed per-Tile work can
therefore matter even when an individual upload is short.

New counters separate allocation/zeroing, file lock/Seek/Read, CRC, CPU hit lookup, worker queue
residence including earlier Tiles, reply backpressure, reply-to-render delay, snapshot dispatch, and
GPU upload. At completion, viewports over 100 ms retain a bounded trace with Generation, Snapshot
sequence, visible/missing counts, CPU hits, uploads, and stage sums/maxima. These sums overlap pipeline
time. The first viewport can include building and device startup; inspect dispatch time and the
separate build/preview report before treating it as an interaction sample.

An 8193 × 4097 generated image used the real HWND/D3D11 path on the RTX 4080 SUPER. Five native-pixel
pans took 6.737–11.246 ms with filesystem-warm misses; repeating the same five with GPU reuse took
0.110–0.149 ms. Tile file reads were P50/P95 0.477/0.566 ms, CRC 0.068/0.088 ms (n=26), and GPU uploads
0.395/1.834 ms (n=27). With an explicitly artificial 20 ms delay per disk Tile, the first pans took
86.282–87.304 ms and GPU reuse took 0.089–0.129 ms. File-read P50 rose to 20.762 ms while CRC was
0.073 ms and reply-to-render P50 was 0.015 ms. The delay exists only in the test executable.
The second-pass assertions also verify that GPU reuse adds no Tile read or upload samples.

This control demonstrates how serialized reads accumulate. It neither reproduces the original
storage conditions nor explains every real tail. No Tile CPU-cache hit occurred in this traversal;
the second pass stayed in GPU memory. The existing CPU-hit counter remains available for a different
eviction workload. Sample counts are too small to characterize rare tails. Ordinary navigation also
passed with 110 GPU-hit and 100 forced CPU-hit selections: P50/P95 0.558/7.132 ms and 0.601/4.358 ms.
This is a regression check, not a controlled performance comparison against an earlier host session.

### Progressive preview proposal — not implemented

Non-interlaced PNG still requires sequential decoding. A complete preview needs pixels through the
last row. Compare these options before changing the implementation:

| Option | Benefit | Cost and limit |
| --- | --- | --- |
| Publish coarse image at EOF before final CRC/sync | Avoid post-scan wait for provisional display | Still waits for all current row writes; source/cache validation can later fail |
| Emit incomplete preview bands during the current scan | Show covered top-to-bottom regions before EOF | Needs explicit uncovered-region state, bounded snapshots, and failure/cancel transitions |
| Preview-only first scan, then a second scan for Tiles | Avoid Tile writes before the first full preview | Reads/decodes the source twice and delays completion; no instant full-preview guarantee |

For a band prototype, the existing Builder would own a preview bounded to 1024² PBGRA pixels
(at most 4 MiB), checked downsample accumulators, and row-coverage metadata. Two queued immutable
snapshots add at most 8 MiB, plus active staging; reserve all of this within the Builder limit.
Count its texture inside the 128 MiB Tile GPU budget. Reuse selection Generation and coalesce progress
updates, initially at no more than two per second. A provisional reply must carry source geometry
without pretending that a validated `DiskCache` exists. On cancellation, source change, or later
validation failure, discard it; never publish partial disk data as a completed cache.

This adds a provisional scene state and new failure transitions. First test bounded write batching
and remeasure preview latency; then review whether bands or a preview-first scan justify that cost.
No new thread, IPC, cache format, budget increase, or progressive scene was implemented here.

## Review regression and Windows GUI checklist

The revised default suite has 63 passing tests and 13 explicitly ignored diagnostics/generators.
Added tests cover free-space rejection while preserving an active cache and unknown directories,
an injected Windows disk-full write error and partial-directory cleanup, 100 source/view replacements,
routing labels, and bounded slow-viewport records. Existing cancellation, abandoned-partial cleanup,
revision/CRC invalidation, reverse/release/focus-loss, and stale GPU-result tests passed again.
These simulate space/write failures; the real volume was not filled. Release probes passed for build
phases, normal/slow viewports, ordinary navigation, hold progress, large-build switch/close, and pixel
readback. The private image was not accessed during this review.

Large-PNG routing now reports `route-to-tiles`; large preloads report `skip-large-preload`. Actual
missing/corrupt-file errors retain `error`. The successful routing decision is no longer a load failure.

Generate owned GUI inputs with `PIXSTRIDE_TEST_ROOT` set to a dedicated directory outside Git:

```powershell
cargo test --release --locked release_prepare_gui_samples -- --ignored --nocapture
$samples = Get-ChildItem -LiteralPath $env:PIXSTRIDE_TEST_ROOT -Directory -Filter 'stage-c-gui-samples-*' |
    Sort-Object LastWriteTime | Select-Object -Last 1 -ExpandProperty FullName
$env:PIXSTRIDE_TILE_CACHE = Join-Path $env:PIXSTRIDE_TEST_ROOT 'gui-cache'
$env:PIXSTRIDE_TILE_CACHE_GIB = '2'
& .\target\release\pixstride.exe (Join-Path $samples 'wide.png')
```

The generator leaves ordinary, wide, slower-build, and invalid PNGs for inspection; delete only that
generated sample directory and its dedicated cache after closing all instances. The following remains
a repeatable checklist. The owner's final acceptance below identifies the six confirmed scenarios;
it does not mark every additional checklist row as manually tested:

| Check | Action and expected result |
| --- | --- |
| First small build | Open `wide.png` with the new dedicated cache; window stays responsive and the full preview appears after construction |
| Pixel layout | Press F and 1; inspect gradient, alpha, all four edges and Tile boundaries without cracks or doubled alpha |
| Pan/zoom | At 100%, drag to both ends and use the wheel repeatedly; coarse fallback may appear, then required Tiles resolve |
| Latest viewport | Pan quickly in opposite directions, release, then Alt+Tab away/back; obsolete work must not move the final view |
| Window lifecycle | Resize continuously, maximize, minimize/restore during loading; recover without black output or deadlock |
| Switch during build | Ctrl+O `build-cancel.png`, then promptly open `ordinary.png`; ordinary display must win and old content must not return |
| Rapid navigation | Alternate ordinary/wide sources and direction keys; final target remains selected and resident caches stay within budget |
| Error recovery | Open `broken.png`, then a valid image; show a non-modal error and continue operating |
| Shutdown | Close during build and Tile loading, then repeat five start/close cycles; process exits and owned partial builds are cleaned |
| Warm reopen | Reopen unchanged `wide.png`; report `hit=true`, with no rebuild; a normal route must not be labeled `error` |
| Source revision | Regenerate an owned fixture after closing it; reopen and confirm stale cache is not reused |
| Idle | After Tiles settle, leave untouched for 30 seconds; compare Present/activity counters, not total counts from prior gestures |

Space shortage, write failure, CRC corruption, and unowned-directory preservation use automated
injection tests; do not fill the real disk or damage a personal cache for manual acceptance. Existing
real-image cache-hit acceptance remains recorded above. Real Device Lost, disk exhaustion, allocation
failure, and long-duration stability remain open. Do not repeat the private first build for
this checklist. All timing still ends at API return, not at a physical display event.

## Final owner-reported Windows manual acceptance

The project owner reported that all six Stage C acceptance scenarios passed on a real Windows desktop.
These are the owner's manual test results, separate from agent-run automated probes:

| Scenario | Result |
| --- | --- |
| Fit, 100%, all four corners, center, and Tile seams | Passed |
| Continuous wheel zoom, mouse dragging, and rapid reversal | Passed |
| Switching JPG/PNG and large PNG in both directions | Passed |
| Resize, minimize/restore, and Alt+Tab | Passed |
| Closing during loading and repeated open/close, without hangs or residual processes | Passed |
| Switching images, cancellation, and closing during construction of a small uncached PNG | Passed |

This is qualitative functional acceptance and adds no new performance samples. The small uncached
construction test does not establish responsiveness or cancellation throughout the private source's
full initial build. All earlier performance measurements and their limits remain unchanged.

Cold-cache full construction remains slow and has no progressive preview. The historical viewport
tail has not been attributed in the real environment. Device Lost, actual disk exhaustion, allocation
failure, and long-duration stability have not been comprehensively validated. Large-image formats,
EXIF Orientation, and ICC/HDR remain limited as documented above. A successful Present return does
not establish completion on the physical display or input-to-photon latency.

## Dependencies and evidence

`png 0.18.1` and `crc32fast 1.5.2` use MIT OR Apache-2.0. Pure Rust decoding avoids a separate libpng
DLL and supplies the row API that the WIC ordinary path does not expose as a guaranteed bounded stream.
The image-rs repository publishes ongoing changes and reports fuzz testing; that is not a security audit
of Pixstride. No nightly features or external AI services are used. The new dependency tree also includes MIT-only
`simd-adler32 0.3.10` and Zlib-licensed `zlib-rs 0.6.8`; other new crates offer MIT/Apache alternatives.
Their original notices are retained in [third-party notices](../../THIRD-PARTY-NOTICES.txt). This covers
Stage C additions, not a complete release inventory. Release size at this checkpoint is 893,440 bytes
versus the Stage B artifact of 596,480; that difference includes code and embedded shaders, not only libraries.

See the [Reader API](https://docs.rs/png/0.18.1/png/struct.Reader.html),
[Decoder limits](https://docs.rs/png/0.18.1/png/struct.Decoder.html), and
[maintainer repository](https://github.com/image-rs/image-png).

Run the explicit memory probe with `PIXSTRIDE_TEST_ROOT` pointing outside the repository:

```powershell
cargo test --release --locked release_streaming_memory_probe -- --ignored --nocapture
```

C1, C2, and C3 validation preceded the completed C4 experiment. Further private-source experiments still require
residency/free-space checks and explicit owner approval before creating more than 2 GiB of temporary cache.
