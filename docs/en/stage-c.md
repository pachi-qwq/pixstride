[English](stage-c.md) | [简体中文](../zh-CN/stage-c.md)

<!-- doc-id: stage-c; revision: 3; status: implementation-review -->

# Stage C: large PNG path

Stages A and B are merged. Stage C has a locally tested streaming cache and Tile renderer. The private
48000 × 32000 PNG has not been decoded at this checkpoint; real-image and manual GUI acceptance remain.

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
encoding or malformed-input allocation pattern. The private extreme image was not read.

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

The private source has only had its file attributes checked. The owner approved a separate 10 GiB
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
Tiles are requested; unusually large viewports use a coarser layer. The Builder's 128 MiB reservation
includes its decoder allowance. OS file cache and allocator overhead are separate. No IPC is introduced.

Selection Generation invalidates source work; the existing Render Snapshot sequence invalidates
viewport work. Cancellation checks run between rows/Tiles. A blocked read cannot be preempted:
shutdown waits up to two seconds, then detaches a worker with no HWND, GPU interfaces, or usable UI-event
access. A later build cleans recognized partial caches. Device failure keeps error-and-exit behavior;
device recreation and actual Device Lost stress remain unverified.

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
separate owner approval. The real experiment has approval for 10 GiB after C3 passes but has not run at
this checkpoint. Manual checks still need real-image seams, alpha, gestures, arbitrary regions,
cancellation, and repeated open/close.

## Dependencies and evidence

`png 0.18.1` and `crc32fast 1.5.2` use MIT OR Apache-2.0. Pure Rust decoding avoids a separate libpng
DLL and supplies the row API that the WIC ordinary path does not expose as a guaranteed bounded stream.
The image-rs repository publishes ongoing changes and reports fuzz testing; that is not a security audit
of Pixstride. No nightly features or external AI services are used.

See the [Reader API](https://docs.rs/png/0.18.1/png/struct.Reader.html),
[Decoder limits](https://docs.rs/png/0.18.1/png/struct.Decoder.html), and
[maintainer repository](https://github.com/image-rs/image-png).

Run the explicit memory probe with `PIXSTRIDE_TEST_ROOT` pointing outside the repository:

```powershell
cargo test --release --locked release_streaming_memory_probe -- --ignored --nocapture
```

C2 must validate a bounded disk pyramid before C3 integration. C4 additionally requires residency and
free-space checks and explicit owner approval before creating more than 2 GiB of temporary cache.
