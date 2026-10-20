[English](stage-b.md) | [简体中文](../zh-CN/stage-b.md)

<!-- doc-id: stage-b; revision: 9; status: merged -->

# Stage B: image browsing core

Stage B merged through [PR #3](https://github.com/pachi-qwq/pixstride/pull/3). It provides JPEG, PNG, and BMP opening, browsing the containing
directory, and presenting decoded pixels through the existing D3D11 render thread. The project owner
confirmed that an image displays, zoom controls work, Left/Right navigation changes images, `Ctrl+O`
opens the native dialog, and window resize remains responsive. The owner also confirmed improved long-hold browsing on `ea23c91`; the acceptance record below
preserves its measurements and gaps. These checks do not cover every error path.

## Opening and navigation

Pixstride accepts one optional image path on the command line, including Unicode paths and paths with
spaces. `Ctrl+O` opens the native Windows file dialog. Left and Right move through supported files in
the current directory; navigation stops at either directory boundary. `F` selects Fit to Window and
`1` selects native pixel size. Fit preserves aspect ratio and centers the image. Native pixel mode
centers the image at 1:1 scale and clips portions outside the client area; ordinary-image panning is not
implemented. Large-PNG zoom/pan is available through the separate merged [Stage C](stage-c.md) path.

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
pending work; an active WIC call can finish. Results outside the approved same-direction hold interval
are discarded when their Generation is stale. The CPU LRU is owned by the worker and keyed by canonical path, file length, and
last-write timestamp. This identity detects common replacements but is not a content hash.

The render thread owns the D3D11 texture LRU and the Immediate Context. It reuses an uploaded texture
on a cache hit, charges the cache by width × height × four bytes, and evicts older non-current entries
to its 512 MiB budget. The CPU cache charges the actual pixel-buffer length. A single decode can add up
to 256 MiB alongside the resident CPU cache; temporary disk caching is not part of Stage B.

The UI sends the latest path to one bounded source-validation thread. This thread canonicalizes the
path and reads file metadata without using WIC. The UI forwards the resolved selection to the render
thread, which checks its texture cache. A valid GPU hit activates that texture without entering the
WIC worker, querying the CPU cache, decoding, or uploading pixels. Only a GPU miss schedules WIC work.
The WIC worker checks the source revision again before its CPU-cache lookup and after decoding.
Metadata validation is a snapshot: a change after the check may be visible on the next selection;
same-length content changes with unchanged timestamps can still collide. There is no file watcher.

Each stage checks the UI-owned selection Generation. The identity thread and WIC worker each keep
only the latest pending selection. A newer Generation clears old GPU probes and pending image updates
except eligible same-direction progress images. The renderer checks the Generation again before submitting an unpresented candidate. Work
already submitted to DXGI cannot be recalled. Directory scanning waits until the current selection's
GPU hit or decode scheduling decision, so it does not start ahead of that decision.

New selections discard queued GPU preloads. The renderer presents the current selection before
uploading preloads, then uploads at most one queued neighbor per wake. The WIC worker starts queued
neighbors only after a 120 ms idle delay. New selections reset that delay and take priority. An active
WIC call, metadata read, directory scan, or texture upload cannot be preempted. In particular, a cache
miss may still wait for an active preload; a GPU hit now bypasses that decoder wait. This retains
Latest Selection Wins: every valid navigation changes the target index, pending work can coalesce,
and the final valid target must eventually reach `Present`. It does not promise every intermediate
frame during key repeat, especially when storage or WIC is slower than the input rate.

Current-image read/decode failures and directory-scan errors update the title bar and stderr rather
than opening a modal message box. Navigation can continue. Over-budget images are still safely
rejected; this change does not raise either cache budget or the per-image limit.

## Diagnostics and limits

### Latest owner-run acceptance record

The owner retested Release commit `ea23c914b6166655f97a6c5bd95d6a174721f5e6` on a real Windows desktop,
using Hardware D3D11, NVIDIA GeForce RTX 4080 SUPER, and Feature Level 11.1. The run used the owner's
existing image directory. Storage type, cloud availability, file-cache state, and the complete input
sequence were not controlled. The owner confirmed that long-hold browsing felt substantially better.
The agent did not read or copy these images; private paths and filenames are omitted here.

| Application boundary | Samples / counts | Result |
|---|---|---|
| Target index changes | 374 | Valid navigation continues to update the target. |
| Image-selection `Present` returns | 229 | Includes 121 hold-progress submissions. |
| GPU-hit selection to `Present` return | 81 | P50 3.426 ms; P95 5.642 ms |
| GPU-miss selection to `Present` return | 148 | P50 50.803 ms; P95 205.110 ms |
| Current Worker service, including checks/cache/WIC | 171 | P95 200.713 ms; maximum 348.118 ms |
| Last released target to its `Present` return | One final release | 248.678 ms |
| Final target / last submitted Generation | 375 / 375 | Final selection completed. |

Seven unfinished-at-release targets entered the release distribution: P50 152.809 ms, maximum
248.678 ms. The final target's trace records 185.022 ms waiting behind an active current decode,
then 69.025 ms of service. These timers have different start events and cannot be added as an exact
decomposition of release latency. No current request entered the active-preload queue bucket.

The longest EXIF query was 170.110 ms; `CopyPixels` reached 250.129 ms. This identifies expensive
stages in this run without proving the cause of the earlier 1.36-second outlier. No second-long Worker
sample occurred here. Different inputs, timing, and cache states prevent a strict before/after
comparison or a claim that all long tails are fixed. GPU-miss timings also include accepted progress
images, measured from each candidate's own selection. `Present` return is an application event,
not actual display latency or Input-to-Photon.

Current GPU hits were 81/230 texture lookups (35.2%), not 81/374 target changes. Current CPU hits were
0/171; GPU-only hits bypass that lookup. Different lookup denominators, evictions, and workloads do
not establish a cache regression. CPU/GPU cache-owned peaks were 536,755,580/536,830,488 bytes, both
below 512 MiB, and returned to zero on shutdown. These counters do not measure total RAM/VRAM or prove
leak freedom. Two current errors rejected a 20,000 × 1,768 image at the 16,384 texture-side limit;
budget rejection reuse counted two hits. Dimension and 256 MiB decoded-byte limits are independent.
Those rejections describe the Stage B build under test. The separate large-PNG preview/tiling path
has since merged in [Stage C](stage-c.md); unsupported formats still reject over-budget images.
The ordinary per-image and CPU/GPU cache budgets remain unchanged.

### Final regression coverage

The ordinary suite has 47 passing tests and three opt-in diagnostics. A new state test calls the
actual navigation functions to check same-direction range retention, sudden reversal, and release of
the previous direction without ending the new direction. Existing tests cover monotonic progress,
pending-result invalidation, budget rejection reuse, and Source Revision changes.

The extended `release_navigation_cache_probe` pauses a current request after `CopyPixels`, before
publishing its result. It then exercises five cases: a newer GPU hit, `WM_KEYUP`, reversal,
`WM_KILLFOCUS`, and opening another file. Each GPU-cached target reaches `Present` before releasing
the gate; afterward, the old decode is cancelled and cannot replace the target. The two messages are
synchronously injected on the HWND's owning thread. File opening calls the accepted-path handler;
it does not automate the native dialog.

The probe also changes a generated BMP's pixel data and length, verifies cache invalidation, deletes
a source and checks non-modal recovery, and rejects truncated JPEG data. The budget test verifies
early rejection, skipping repeated WIC work, and retry after a source revision change. All fixtures
are self-generated outside Git; no private image or unavailable cloud file is read.

The cache/boundary, hold-progress, and JPEG probes passed separate Release reruns. Fmt, Clippy with
warnings denied, unit tests, Release build, Rustdoc, the 52-file docs check, 41 docs-tool tests, and
`git diff --check` passed. Code additions are test-only; production threading, scheduling, resource
ownership, and the 256 MiB / 512 MiB / 512 MiB budgets are unchanged. Basic GUI and improved long-hold
usability are owner-confirmed. Physical keyboard/Alt+Tab boundary stress, actual blocked-cloud shutdown,
long-duration pressure, Device Lost injection, controlled startup, and displayed-frame timing remain
unverified. Injected messages and worker gates do not replace that testing.

### Earlier diagnostic runs

The owner's subsequent run on `89bb029` verified fast individual presses but still observed pauses
during key repeat and a delayed final update after releasing the key. The 56 GPU-hit samples were
3.810 ms P50, 5.194 ms P95, and 5.375 ms maximum. The 24 worker requests had 132.920 ms queue-wait
P95 and an 839.686 ms maximum; service P95 was 365.662 ms, maximum 1.273 s. The combined WIC-open
stage reached 1.270 s, while `CopyPixels` reached 196.640 ms. The active-preload queue bucket contained
one 23.270 ms sample, so these aggregates do not support blaming preloading for the longest wait.
The final target and submitted Generation were both 125. A delayed final update may therefore be
the latest valid selection completing, rather than a replay of queued selections; the aggregates do
not correlate it with the reported key-release instant.

The stage-cancellation follow-up kept the same threads and budgets. It checks selection validity between WIC stages,
including after opening and before allocation/`CopyPixels`. An obsolete task returns a distinct
cancellation result instead of continuing avoidable work; it is not counted as a corrupt-file error.
A synchronous WIC call already in progress is still non-preemptible. This change cannot guarantee
continuous frame updates during uncached key repeat or eliminate a blocked open.

WIC open now has Factory creation, `CreateDecoderFromFilename`, and `GetFrame`/EXIF submetrics.
The exit report retains at most 16 slow/error task records, keeping eight recent and up to eight slower
older records, with filename only, selection Generation,
current/preload role, queue/service times, the active current Generation observed at enqueue, outcome,
and that task's measured stages. The threshold is 100 ms for current-request queue wait or any service;
actual errors are also retained. Intentional preload idle delay alone does not qualify. Stage-boundary
cancellations have a separate counter. These records are collected in memory and printed only on exit;
they are diagnostic evidence for the next real-directory run, not a claim that the long tail is fixed.

On a navigation `WM_KEYUP`, the UI records that instant's target Generation. The report distinguishes
an already-presented target, one presented after release, one still waiting, and one superseded by later
input. The release-to-`Present` distribution contains only targets unfinished at release that later
complete without being superseded. This does not cancel the final target on key release or change the
navigation rate; it distinguishes delayed completion from subsequent navigation, using application
events rather than display timing.

On exit, the bounded timing collector reports directory scan, WIC open, converter setup,
`CopyPixels`, total WIC processing including failed paths, texture upload, render-thread request delay, first image
`Present` return, and CPU/GPU cache hit counts with current and peak resident bytes. These byte counts
cover cache-owned entries, not process working memory or driver allocation overhead. WIC owns file
reading, so this version does not isolate storage latency from decoder work. `Present` return is an application-side boundary and does not
measure when photons reach the display.

The current diagnostic build also reports current-selection and preload CPU/GPU hit and miss counts,
source-identity time and CPU-cache lookup time separately, current request-to-worker-result time, and separate GPU-hit
and GPU-miss selection-to-`Present`-return distributions. It separately measures time waiting in the
single WIC worker queue and time spent servicing a current request, including source checks, cache
lookup, and any decode. Hit rates use successful cache lookups as
their denominator; failed source checks do not enter a cache lookup; a successful miss is counted even if decoding
later fails. GPU miss counters are recorded when a decoded result reaches texture lookup/upload, so
a failure before that point is absent from that denominator. GPU hits use the separate source-validation path above. The previous owner run
reported aggregate CPU hits/misses
of 26/13 and GPU hits/misses of 26/5, including preloads; this corresponds to 66.7% and 83.9% aggregate
hit rates for that run, not navigation-only rates. Its 11 image-request-to-render samples had a 202 µs
P50 and a 161.58 ms maximum. A later owner run used an owner-selected directory with unverified storage type and collected 78 current
requests and 143 preload requests. Current-selection CPU hits were 73/78 (93.6%); GPU hits were 66/76
(86.8%). Preload CPU hits were 66/143 (46.2%), and preload GPU hits were 65/134 (48.5%). The different
denominators reflect successful cache lookups; a request failing before its cache lookup is absent from that denominator.

In that run, source-identity and CPU-cache lookup took 3.072 ms P50 and 4.153 ms P95. Current request
to worker result took 3.472 ms P50 and 126.214 ms P95 (376.136 ms maximum). A current GPU hit from
selection to `Present` return took 3.456 ms P50 and 4.305 ms P95. A GPU miss took 51.537 ms P50 and
177.570 ms P95. WIC full decode took 48.462 ms P50 and 212.795 ms P95; preload decode reached
1.205 s maximum. The CPU and GPU cache-owned peak bytes were each about 511.5 MiB, close to their
512 MiB budgets; these values do not represent total process RAM or actual driver VRAM use.

A rapid key-repeat run later recorded 233 input events, 146 current worker results, and 87 image
requests handled by the render thread. Current request-to-worker-result latency had a 207.604 ms P95
and a 1.200 s P99 (1.232 s maximum). Current GPU-hit selection-to-`Present` latency had a 10.267 ms
P95. The difference points to work before rendering, but this run did not separate worker queue delay
from request processing. That build coalesced pending selection requests and discarded stale generations;
it prioritizes the latest choice and does not promise to display every intermediate image during key
repeat. The subsequent run below separates those intervals.

One selected image was rejected before allocation because its estimated BGRA buffer was 394,516,200
bytes (about 376 MiB), above the 256 MiB per-image CPU budget. This is an intentional Stage B resource
limit, not a decryption or file-corruption error. Supporting larger decoded images requires a revised
memory budget or the later large-image path; the current limit has not been raised.

A later owner run at the pre-optimization revision `0250da7` recorded 671 input events. Worker queue
wait was 10.449 ms P50, 30.921 ms P95, 62.965 ms P99, and 139.380 ms maximum. Worker service was
46.726 ms P50, 234.616 ms P95, 306.615 ms P99, and 1.225 s maximum. The latter outlier belongs to
service, not queue wait. The older WIC stage timers only recorded successful decodes; failed opens,
budget rejection, allocation, and final source verification could be missing from those breakdowns.
The aggregate report cannot identify the cause of that sample, nor establish SSD-versus-cloud effects.

The revised collector records WIC stages even when `?` returns an error. It separates source identity,
CPU-cache lookup, WIC open, orientation/layout checks, pixel allocation/zeroing, converter setup,
`CopyPixels`, and final revision verification. Current and preload WIC totals have separate
distributions. A conditional queue metric marks requests enqueued while a preload was already running;
it is not an exact decomposition of every scheduler delay. Navigation target changes and successful
image-selection `Present` returns are counted, and the report prints the latest target and last
presented Generation. A GPU-only hit produces no current CPU-cache sample; its absence is intentional.

### Approved progress display during a hold

The owner's `e5c3a2a` retest still found visible pauses. There were 223 target changes but only
73 image-selection `Present` returns; 77 obsolete tasks stopped between stages. Worker queue wait
was 8 µs P50, 24.804 ms P95, and 86.586 ms maximum. Service was 31.129 ms P50,
189.628 ms P95, and 1.371 s maximum. The grouped `GetFrame`/EXIF stage reached 1.358 s,
while `CreateDecoderFromFilename` reached 31.134 ms and `CopyPixels` reached 303.545 ms.
This narrows one long tail to frame/metadata handling; it does not establish its underlying cause.
The old recent-only trace buffer lost that early task, so the report cannot identify its file.
GPU hits remained fast in that run: n = 54, P50 3.475 ms, P95 4.263 ms, maximum 4.413 ms.
The five unfinished-at-release targets completed within 128.485 ms; final target/submitted
Generation both reached 224. Those samples do not explain every perceived pause during the hold.

The owner approved a bounded exception to stale-result rejection: while the same navigation direction
remains active, an already-running current decode may produce a progress image. The UI maintains the
first allowed Generation for that direction and the latest target using the existing counter. The
renderer accepts only nondecreasing Generations in that interval. A progress image changes displayed
pixels and the title, never the target index/path. The title identifies the displayed path and pending
target. Pending requests still coalesce; there is no queue of every skipped image or input throttling.

Key release, reversal, focus loss, and file opening close the previous interval. The latest target keeps
loading; older results cannot subsequently be accepted as progress. A newer GPU-cache activation also
prevents an older decode from replacing it. All GPU resources remain on the render thread. Frames
already submitted to DXGI cannot be withdrawn. Errors for obsolete progress candidates are suppressed;
errors for the current target remain non-modal. Uncached final targets may still wait for synchronous
WIC work, and a target rejected by the budget reports an error rather than displaying new pixels.

Raw dimensions are now checked immediately after `GetFrame`, before EXIF lookup. `GetFrame`, EXIF
lookup, and this budget preflight have separate timers. A worker-owned cache retains at most 32 typed
budget rejections by validated Source Revision, so selecting an unchanged rejected source skips WIC.
A changed length or timestamp retries decoding. Cloud/read failures and corrupt-image errors are not
cached as budget rejections; the weak metadata-key limitation remains. The 16 task records now retain
the newest eight and up to eight slower older records, rather than losing a long tail to fast errors.

The Release test `release_hold_navigation_probe` uses 40 generated 64 × 64 BMPs and hardware D3D11.
It injects 80 ms of non-preemptible worker work after `CopyPixels`, changes targets every approximately
20 ms in each direction, and verifies progress and final completion after release. A test-only strict
Generation mode disables the progress interval to reproduce the earlier discard policy. Each variant
was run once; startup, OS scheduling, and cache effects are uncontrolled. No real keyboard events,
private images, or cloud files were used.

| Same injected workload | Strict Generation reference | Progress display |
|---|---|---|
| Target changes | 60 | 60 |
| Image-selection `Present` returns, including first/final images | 4 | 23 |
| Progress `Present` returns | 0 | 13 |
| Final target / submitted Generation | 61 / 61 | 61 / 61 |
| CPU / GPU cache-owned peak bytes | 49,152 / 49,152 | 245,760 / 245,760 |

The progress run's worker service P50 was 81.167 ms, including the injected delay; this is not faster
decoding. Its only unfinished-at-release target completed after 108.739 ms; n = 1 cannot support
release-latency percentiles. The strict run had two such samples, 94.205–119.231 ms. Progress counts
compare the submitted image Generation with the target observed when recording the successful return.
The result verifies that completed work can update the image during a hold. The later owner retest
above confirms improved real-directory usability; reproducible second-long WIC stalls still need investigation.

The cache regression was rerun: GPU-hit selection-to-`Present`, n = 103, was P50 762 µs,
P95 7,160 µs, P99 8,015 µs, maximum 9,988 µs. The 100 forced-GPU-eviction CPU hits were
P50 799 µs, P95 5,999 µs, P99 7,818 µs, maximum 7,869 µs. CPU hits were 100/103 (97.1%);
GPU hits were 103/205 (50.2%), including deliberate evictions. Worker queue P50/P95/P99/max
was 10/67/1,052/1,546 µs, and service was 117/429/4,529/5,817 µs. Cache peaks remained
786,432 CPU bytes and 1,048,576 GPU bytes. The two successful uncached WIC selections remain too
few for meaningful miss percentiles. These are application-side fixture results, not display latency.

A separate eight-pair probe on a generated 4096 × 3072 noisy JPEG compared full decode with a native
1024 × 768 decode. Full-decode P50 was 115.921 ms; native-scale P50 was 100.128 ms. This small
experiment does not justify a new preview/quality lifecycle, so native shrinking is not used in the
viewer. WIC's [closest-size contract](https://learn.microsoft.com/en-us/windows/win32/api/wincodec/nf-wincodec-iwicbitmapsourcetransform-getclosestsize)
and [native copy contract](https://learn.microsoft.com/en-us/windows/win32/api/wincodec/nf-wincodec-iwicbitmapsourcetransform-copypixels)
require decoder-negotiated size and format. The probe does not establish performance for other images. A second eight-pair run had P50 values
of 136.405 ms full and 121.634 ms scaled; this variation reinforces the limited conclusion.

Reproduce the progress test after the existing cache probe:

```powershell
cargo test --release --locked release_hold_navigation_probe -- --ignored --nocapture
# Test-only reference mode; remove it before the progress run.
$env:PIXSTRIDE_TEST_STRICT_GENERATION = "1"
cargo test --release --locked release_hold_navigation_probe -- --ignored --nocapture
Remove-Item Env:PIXSTRIDE_TEST_STRICT_GENERATION
```

Keep `PIXSTRIDE_TEST_ROOT` set to a writable directory outside the repository. The ordinary test suite
now has 47 passing tests and three opt-in diagnostics; the hold, cache, and JPEG probes were explicitly
run. Regression coverage includes monotonic progress, unchanged targets, release/reversal rejection,
newer GPU-hit precedence, budget preflight before EXIF, bounded rejection reuse/source changes, and
retaining a worst task across repeated fast errors. Latest owner-run acceptance is recorded above; the full physical-keyboard focus/reversal stress matrix remains incomplete.

### Release diagnostics on generated local samples

The reproducible test `release_navigation_cache_probe` uses three generated 256 × 256 BMP files,
hardware D3D11, and warm Windows file caching. It completes 100 alternating selections and then
submits 200 selections without waiting for intermediate frames. The baseline uses `0250da7` plus
test-only instrumentation outside the working repository; the new build uses this PR's changes.
These are single diagnostic runs, not a controlled storage benchmark or evidence of real-directory
key-repeat smoothness. No private image or OneDrive file was opened for these runs.

| Boundary | Baseline | Revised path |
|---|---|---|
| GPU-hit selection to `Present` return, n = 101 | P50 287 µs; P95 6,662 µs; P99 6,749 µs; max 6,769 µs | P50 288 µs; P95 6,631 µs; P99 6,750 µs; max 6,855 µs |
| Current selections serviced by WIC worker | 105 | 1 |
| Cache-owned CPU / GPU peak | 786,432 / 786,432 bytes | 786,432 / 786,432 bytes |

The small-file idle latency is effectively unchanged in this sample. The verified change is avoiding
the decoder handoff for GPU hits. The new test also holds the WIC worker behind a test-only gate until
five GPU-hit selections have completed, proving they do not wait for that worker. A further run adds
file replacement/deletion checks: GPU-hit latency, n = 102, was 299 µs P50, 6,379 µs P95, 7,197 µs
P99, and 83,352 µs maximum. That run also recorded an 83,220 µs source-identity read. The aggregate
timers do not prove those two samples are the same request. The final requested Generation matched
the last successful image `Present` Generation, and file deletion produced one non-modal error before
the next valid selection completed. The two uncached selections took 193.002 ms at startup and
5.882 ms after a revision change; n = 2 does not support useful miss percentiles.

A subsequent extended run forcibly evicts non-current GPU entries only in the test build, leaving the
CPU cache intact. Its 100 CPU-hit/GPU-miss selections reached `Present` return at 527 µs P50,
5,387 µs P95, 7,971 µs P99, and 8,051 µs maximum. Across that run, 102 GPU-hit samples were 359 µs
P50, 6,767 µs P95, 7,356 µs P99, and 7,780 µs maximum. The 102 decoder-service samples were 105 µs
P50, 179 µs P95, 4,350 µs P99, and 5,083 µs maximum; queue wait was 8 µs P50, 13 µs P95,
62 µs P99, and 111 µs maximum. The two WIC-current selections are too few for miss percentiles.
There were 401 target-path changes and 204 successful image-selection `Present` returns; final target
and submitted Generation were both 404. Current CPU hit rate was 100/102 (98.0%), and current GPU
hit rate was 102/204 (50.0%) because this test deliberately evicts textures. CPU/GPU cache peaks were
786,432/1,048,576 bytes. These rates describe the injected workload, not real browsing. The test sends
selection requests directly and does not produce a real keyboard-input sample count.

Reproduce the hardware test in a Windows desktop session, with a local writable fixture directory
outside the repository:

```powershell
$env:PIXSTRIDE_TEST_ROOT = "<local-test-directory>"
cargo test --release --locked release_navigation_cache_probe -- --ignored --nocapture
```

The test creates only its own small fixtures. It does not measure keyboard delivery or screen output.
The ignored test is run explicitly on Windows with D3D11; ordinary CI runs the remaining regression
tests without depending on an interactive GPU session.

### Manual repeat-navigation comparison

Use the same reviewed small JPEG/PNG/BMP set on local SSD and, separately, in a fully downloaded
OneDrive directory. Record cloud availability and storage location; do not infer storage type from a
drive letter. Run the Release executable, wait for the first image and neighbors, and perform these
separate runs: 30 short alternating presses, a 3-second hold in each direction, and rapid reverse
navigation. After each run release the key, record the target filename, verify the final image, wait,
then close normally to collect timings. During a hold, check that progress images advance in the same
direction; after release/reversal/focus loss, check that no older progress candidate replaces the latest
selection. Compare the new progress counter as well. Include a budget-rejected image and verify that the title-bar
error allows further navigation without dismissing a dialog. Repeat with a known unavailable cloud
file only if its read/download behavior is acceptable to the owner.

Compare source-validation time, active-preload queue waits, WIC current/preload service, GPU hit/miss
selection-to-`Present`, target changes, presented selections, and cache peaks. A GPU-only hit skips WIC
and CPU lookup; CPU-hit requests still wait for the single WIC worker and texture upload if the GPU
entry was evicted. Sustained key repeat has the owner-run evidence above. Controlled cloud latency, real-directory
GPU-eviction/CPU-hit distributions, shutdown during an actual long cloud/WIC call, and a controlled
comparison of the 120 ms preload policy still need further runs.
`Present` return remains an application boundary; displayed-frame timing needs separate instrumentation.

The automated suite includes path sorting and boundaries, checked image layout and budgets, EXIF
mapping, CPU cache eviction, stale-generation replacement, cancellation of queued preloads, viewport
sizing, rejection of truncated JPEG data, a generated BMP decoded through WIC, and generated JPEG/PNG
samples encoded and decoded through WIC. The owner has verified basic real-image presentation, zoom,
navigation, dialog opening, and resize. Slow or cloud-backed file reads, rapid reverse navigation during a long decode, and closing
while a WIC call is still running need additional manual testing. WIC calls are synchronous and cannot
be cancelled once started. On shutdown, Pixstride requests cancellation and waits up to 2 seconds; if
a worker remains in WIC or a filesystem call, it detaches the thread and exits the process so Windows can reclaim its
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
