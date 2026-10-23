[English](stage-d1b2.md) | [简体中文](../zh-CN/stage-d1b2.md)

<!-- doc-id: animation-playback; revision: 2; status: animation-playback-review -->

# D1-b2: GIF playback on the D3D11 render thread

This local branch connects the merged [D1-b1 producer](stage-d1b1.md) to the Viewer. Multi-frame GIFs
play after the static poster; single-frame GIFs keep the poster path. Windows automated checks pass.
The project owner reports all 8/8 Windows manual acceptance scenarios passed. Code review and PR
merge remain pending. There is no public Release or new dependency.

## Ownership and handoff

`ImageLoader::take_animation` transfers the unique lazy controller to the existing Render Thread.
One reusable MTA Worker still owns all WIC COM objects, the canvas, patches and Restore Previous data.
Only owned PBGRA8 pixels and immutable metadata cross the thread boundary. JPEG/PNG/BMP and the
Stage C Tile Worker keep their existing execution models.

The renderer accepts only the final stable file selection, checking its Generation, Source Revision,
Session ID, `GifAnimationV1` content type and Frame Ordinal. It does not activate every progress GIF
during held-key navigation. Cancellation revokes old output immediately; checks run again before
upload and before Present submission. Commands already submitted to DXGI cannot be retracted.
Reselecting the same GIF retires its previous animation resource before reusing the poster.

The ordinal-zero output supplies raw metadata and is checked against the poster's revision. Its
pixels are not uploaded again. The poster's successful Present-return boundary starts the first delay.
The renderer retains at most one future `Arc<FrameOutput>`; the producer can use the other Credit.
Queued, in-production and consumer-held outputs together still have at most two Credits. There is
no additional upload queue. The final Arc releases its CPU bytes and Credit.

## GPU resource and budget boundaries

One DEFAULT BGRA8 Texture2D/SRV is updated with each accepted full PBGRA8 canvas. Animation frames
never enter the static GPU LRU. The Immediate Context and all GPU resources stay on the Render Thread.
After `UpdateSubresource` returns, the CPU frame may be released: the API has copied its source data.
Same-context command ordering handles Draw/update hazards; a frame-latency event is not a completion
fence. Contention may cause driver-owned temporary copies. See Microsoft's
[UpdateSubresource contract](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-updatesubresource).

On replacement, the renderer unbinds the SRV and places an EVENT Query after its last use. The old
texture and its charge survive until `GetData` returns a true completion value; an `Ok` result alone
is insufficient because S_FALSE can map to it. Collection runs during real work, never through idle
polling. This follows the [EVENT Query contract](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_query).
At most two animation texture objects, including retired ones, are retained. A completed
finite animation retains its last texture until selection changes or shutdown.

| Resource | Limit and accounting |
|---|---|
| Ordinary single image | 256 MiB, unchanged. |
| CPU pixels | Shared 512 MiB ledger; animation uses at most 256 MiB inside it, including canvas, patch, saved region and two outputs. |
| GPU image payload | Shared 512 MiB; animation active/retired payload uses at most 128 MiB inside it. Static admission counts animation bytes and evicts eligible unpinned entries before allocation. |
| Stage C | Existing independent CPU/GPU/disk budgets and cache format, unchanged. |

Insufficient animation quota stops that session with a non-modal error, retaining the poster or
last valid frame. There is no unbounded retry. Payload accounting does not hard-bound Working Set,
WIC allocations, D3D11 driver copies, swap-chain buffers or physical VRAM. Static LRU retirement
retains its existing D3D11 runtime semantics; animation retirement is explicitly charged through Query.

## Clock and compatibility policy

The Render Thread owns a virtual-testable monotonic clock. Its wait set contains work first, then
eligible producer output, a single-shot Waitable Timer and DXGI frame availability when a draw is
pending. No per-frame thread or Sleep polling is used. Normal deadlines advance along the previous
deadline so wakeup jitter does not accumulate. If a whole next interval has already elapsed, or a
decoder underflow occurred, timing rebases instead of repeatedly submitting catch-up frames.

If the next frame is missing, the current frame stays visible. Underflow is counted once and the
timer is removed; only new data or user/window work wakes the renderer. Sequential composition never
skips Disposal dependencies. This implementation does not discard decoded frames to chase a fixed
wall-clock position; sustained slow decoding slows playback. Its skipped-display counter is zero by
design, separately from stale-session rejection. The owner accepted short-delay pacing on the tested build.

| Raw metadata | Branch display policy |
|---|---|
| Delay 0 or 1 centisecond | 100 ms. |
| Delay 2 / 3 / 10 centiseconds | 20 / 30 / 100 ms. |
| No Loop extension | One pass. |
| Loop = 0 | Repeat until cancelled. |
| Loop = n, positive | First pass plus n repetitions. |

GIF89a defines nonzero Delay in hundredths of a second; this alone does not mandate the zero-delay
compatibility rule. This policy follows independently inspected
[Chromium delay handling](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/platform/graphics/deferred_image_decoder.cc)
and [Firefox FrameTimeout](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/image/FrameTimeout.h).
Loop interpretation was checked against Firefox's
[NETSCAPE parser](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/image/decoders/nsGIFDecoder2.cpp)
and deterministic browser samples. Raw Worker metadata is unchanged. The owner accepted the current
short-delay pacing and Loop behavior in manual testing; code review remains pending. The earlier
20 ms prototype suggestion is not used. These checks do not establish compatibility with all encoders.

On 2026-10-22, isolated headless Edge 155.0.4283.62 and Firefox 153.5.0 displayed self-authored
three-color GIFs for 4.5 seconds. Host-timestamped screenshots sampled compositor pixels, not photons.
Firefox took 755 samples, with screenshot-call P50 1.502 ms; observed transition-interval medians for
raw 0/10/20/30/100 ms were 97.33/97.33/18.06/28.68/97.33 ms. Edge took 77 samples, call P50
62.196 ms; its 20/30 ms cases are undersampled and cannot establish exact durations. Its 0/10/100 ms
cases behaved alike. Both observed no-loop/1/2 fixtures stopping after one/two/three passes and a
zero-loop fixture continuing. This is compatibility evidence, not a browser benchmark or a guarantee
of the same on-screen cadence in Pixstride. No external decoder source was copied.

The last frame remains for its complete normalized Delay. Finite completion cancels future deadlines
and retains the final image. The producer may speculatively prepare up to its two-Credit bound before
consumer cancellation. It always resets its canvas at a pass boundary.

## Pause and lifecycle

Space toggles the user's play intent only for an active animation; key repeat is ignored. Minimize
and the native file dialog temporarily pause; restore/cancel preserves an existing manual pause.
Losing focus continues playback and still releases held navigation state. Pause retains the current
frame's remaining delay; resume does not account for time spent paused. An already accepted upload
may finish its submission boundary before pause acknowledgement; it does not create a periodic loop.

Changing files invalidates the session before old frames can qualify again. Session, decode, timer
creation and texture-admission errors are non-modal. Device/Present failures use the established
safe shutdown path; full Device Lost recovery is still out of scope.

On exit the renderer cancels production, releases held frames, keeps GPU teardown on its own thread,
and transfers the CPU-only controller back to UI cleanup. UI waits for the Worker completion event
while continuing its message loop, then joins after COM/canvas/queue cleanup. A post-CopyPixels gate
test proves Render can exit while the Worker remains held. Credit/pause waits are cancellable.
WIC synchronous calls cannot be forced to return. The existing 2-second shutdown grace may detach
the single still-owned Worker for process exit; this is not a fixed WIC cancellation guarantee.

## Automated evidence and measurements

The default suite passes 95 tests; 31 explicit probes are not included in that run. Playback adds
virtual-clock, session/order, pause-intent and invalid-state checks. Release tests use actual WIC
and hardware D3D11. Ten GPU uploads reuse one Texture; 120 pixel positions across two passes match
an independent coordinate oracle, including transparency and Disposal 0–3. Credit/byte release,
shared-budget refusal, Query retirement, finite loops, pause/dialog/minimize, stale-output handling,
slow-Worker underflow, static navigation and close are tested.

Twenty Release diagnostic groups passed, including preparation and five playback workloads.
Existing poster, EXIF GPU readback, JPEG/PNG/BMP cache/navigation, held-key reversal, synthetic
20000 × 65 PNG Viewer and 17,425-pixel Tile readback regressions passed. Twenty controlled static
GPU hits performed no new WIC CopyPixels. Finite completion, pause and static idle had zero extra
wakeups/Present submissions in gated 220 ms observation windows. These windows do not prove
long-duration idle behavior. Three separate runs of the actual Release executable closed while
playing, paused and minimized; process exit took 24.122/29.049/25.651 ms after WM_CLOSE (one sample
each). Final animation bytes/Credits were zero; no shutdown detachment was used. No private extreme PNG or its cache was accessed.

Held-navigation retesting exposed a batch handoff defect: after `take_pending`, key release can make
an old progress image obsolete while a newer GPU probe remains valid in the same batch. The former
whole-batch `continue` lost that probe, leaving UI waiting for a Miss and the final decode never queued.
The fix discards only the obsolete image and also preserves subsequent work after image-upload failure.
A gated test failed before the fix and passed after it. Sixty-four repeated gated runs and eight
held/reverse runs across four processes passed without changing timeouts or weakening assertions.
UI play availability and GPU views are also scoped to the current selection Generation.

<!-- measurements-start -->

Timings are microseconds; triples are P50/P95/P99.

| Workload | Decode n | WIC decode | CPU compose | Output copy | GPU upload | Deadline Lateness |
|---|---|---|---|---|---|---|
| 4 × 3 / 5 frames | 63 | 84/125/144 | 0/0/1 | 0/0/1 | 8/94/152 | 7825/15157/15372 |
| 512 × 256 / 128 frames | 63 | 806/1077/1148 | 64/142/204 | 66/82/120 | 29/39/263 | 7887/15221/16001 |
| 2048 × 1024 / 4 frames | 6 | 9944/10503/10503 | 956/1984/1984 | 1038/1057/1057 | 854/979/979 | 7621/11119/11119 |
| 512 × 512 / 128 local frames | 63 | 123/194/224 | 5/13/21 | 190/228/325 | 48/77/324 | 7257/14622/15011 |
| 512 × 256 / short delays | 63 | 812/1161/1274 | 71/140/183 | 70/104/208 | 30/51/336 | 7555/14543/15372 |

| Workload | Shared CPU peak bytes | Animation CPU peak bytes | Animation GPU peak bytes | Process peak WS bytes | Max sampled Private Usage bytes |
|---|---|---|---|---|---|
| 4 × 3 / 5 frames | 240 | 192 | 48 | 49106944 | 60194816 |
| 512 × 256 / 128 frames | 3145728 | 2621440 | 524288 | 54317056 | 65425408 |
| 2048 × 1024 / 4 frames | 50331648 | 41943040 | 8388608 | 114491392 | 110608384 |
| 512 × 512 / 128 local frames | 4227072 | 3178496 | 1048576 | 56692736 | 69865472 |
| 512 × 256 / short delays | 3145728 | 2621440 | 524288 | 54714368 | 66895872 |

| Workload | Selection → CPU output (n=1) | Selection → updated Present (n=1) | CPU output → updated Present (n=1) | Present P50/P95/P99 | Shutdown → cleanup observed (n=1) |
|---|---|---|---|---|---|
| 4 × 3 / 5 frames | 162871 | 270900 | 108028 | 67/93/798 | 12108 |
| 512 × 256 / 128 frames | 186594 | 280707 | 94113 | 64/95/5882 | 13395 |
| 2048 × 1024 / 4 frames | 210391 | 300209 | 89818 | 126/878/878 | 14268 |
| 512 × 512 / 128 local frames | 169299 | 276212 | 106913 | 69/101/730 | 15643 |
| 512 × 256 / short delays | 186211 | 281486 | 95275 | 60/99/8066 | 16755 |

<!-- measurements-end -->

Measurements were collected on 2026-10-22–23. All timings above are application-side events from Release x64, Rust 1.96.0, windows-rs 0.62.2,
system WIC and an RTX 4080 SUPER, Feature Level 11.1. Each workload runs in its own process after
separate preparation; OS caches are not cleared. The small/medium/partial/short cases stop after
60 updated-frame Presents; the large finite sample has three updates. Decode counts can include
bounded speculative or cancelled work. Small-sample P99 values are maxima, not population estimates.
Private Usage is sampled at phases, not continuously peaked; process counters include the driver,
DLLs, allocator and test harness. Every completed probe has zero animation bytes/Credits outstanding. Shutdown observation includes
prior Render/static-worker cleanup in the profile harness, so it is an upper bound on Worker-only
completion time. Producer and Renderer timing reports have separate scopes.

Deadline Lateness ends before upload, while Present duration and selection-to-Present end at the API
return. Ordinary timer granularity and scheduling contribute to these application delays; no causal
trace isolates them here. Present return, UpdateSubresource return and DXGI frame availability have
different meanings. None measures Input-to-Photon. Use PresentMon/ETW or external display measurement
for that separate question. D1-b1 timings provide context, not a controlled before/after comparison.

### Ordinary-image comparison

A detached, unmodified `1073e09622e39c14e33edb7d833c7ad6824228ad` checkout and this branch ran
the existing navigation/cache probe in three alternating fresh-process pairs. Both generated the
same three 256 × 256 BMP payloads on local storage. The current probe uses the production animation
controller handoff, verifies zero animation starts for ordinary images, and keeps the existing
no-new-WIC assertion. Each run includes 100 sequential cache hits plus the probe's rapid-navigation
tail; all six recorded 110 hit-to-Present samples. OS caches were not cleared.

| Build | Run | n | P50 µs | P95 µs | P99 µs |
|---|---|---|---|---|---|
| baseline | 1 | 110 | 268 | 6565 | 7079 |
| playback | 1 | 110 | 262 | 6358 | 6801 |
| baseline | 2 | 110 | 264 | 6033 | 6863 |
| playback | 2 | 110 | 269 | 6206 | 6660 |
| baseline | 3 | 110 | 288 | 6295 | 6770 |
| playback | 3 | 110 | 307 | 6067 | 6533 |

This small controlled comparison shows no obvious static-path regression: baseline P50 ranged
264–288 µs and playback P50 262–307 µs; P95 ranges overlap. It is not evidence of a speedup or a
population-level performance guarantee. The metric ends at Present return, not display completion.

## Reproduce and review

Choose a dedicated local `PIXSTRIDE_TEST_ROOT` outside the repository. Prepare self-authored samples,
then run probes in separate processes:

```powershell
cargo test --release --locked release_animation_prepare -- --ignored --nocapture
cargo test --release --locked release_animation_gpu_frames -- --ignored --nocapture
cargo test --release --locked release_gif_playback_viewer -- --ignored --nocapture
cargo test --release --locked release_animation_stale_underflow -- --ignored --nocapture
$env:PIXSTRIDE_PLAYBACK_CASE = 'medium'
cargo test --release --locked release_gif_playback_profile -- --ignored --nocapture
cargo build --release --locked
& '.\target\release\pixstride.exe' (Join-Path $env:PIXSTRIDE_TEST_ROOT 'animation-fixtures\medium.gif')
```

Other prepared profile cases are `small`, `large` and `partial`. The prepared `short` case sets generated GCE delays to 0/1/2/3/10 centiseconds; it is not an external
sample. `loop-once`, `loop-twice` and `loop-thrice` support manual finite-loop checks. Build and
test logs report distributions, process counters and final charges.

### Owner-run Windows manual acceptance

The project owner reports all 8/8 scenarios below passed on the real Windows desktop for code commit
`f7ad89465c4e3245bf70faa5a841b71c18c327a0`. The subsequent acceptance-record changes only update
documentation and pair metadata. These are owner-provided functional and visual observations, not
pixel-level correctness proofs, precise timing measurements or long-duration CPU-utilization guarantees.

| Scenario | Owner-reported result |
|---|---|
| Normal GIF playback. | Passed. |
| Short Delay pacing. | Passed. |
| No Loop extension, finite Loop and infinite Loop. | Passed. |
| Space pause/resume. | Passed. |
| Minimize, restore, Alt+Tab and Ctrl+O. | Passed. |
| Navigate between GIF and JPEG/PNG/BMP or a generated large PNG. | Passed. |
| Held/reverse navigation, Resize, Fit/100% and close during playback. | Passed. |
| No sustained abnormal utilization observed while paused, after playback ends or during static idle. | Passed. |

The automated evidence and performance boundaries above remain separate from this manual record.
Code review and PR merge are still pending.

Broad GIF encoder compatibility, User Input Flag semantics, blocked storage/codecs, Device Lost,
actual allocation failures, long-duration stability and Input-to-Photon remain incomplete. There is
no seek, speed control, reverse playback, APNG/WebP animation, HDR/ICC work or Stage C cache change.
The GIF file admission limit remains 64 MiB. Structural preflight cannot guarantee LZW correctness.
Poster/animation canvas and Disposal 2 use the explicitly tested transparent-clearing policy; global
background and Viewer background remain distinct. Manual acceptance does not establish identical
background or timing behavior for every real-world GIF encoder.
