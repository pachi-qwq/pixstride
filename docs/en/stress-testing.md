[English](stress-testing.md) | [简体中文](../zh-CN/stress-testing.md)

<!-- doc-id: stress; revision: 3; status: proposal -->

# Cross-format stress test plan

This plan covers compatibility, large images, resource exhaustion, and malformed input. A planned
case is not a supported feature or a completed test. The only existing extreme sample is a private
non-interlaced RGBA8 PNG, 48000 × 32000 pixels, with a file length of 2,071,102,933 bytes.
Stage C completed its full streaming scan, disk Tile Pyramid, viewport tests, and six owner-run Windows
manual acceptance scenarios. Results and remaining stress gaps are in [Stage C](stage-c.md); this does
not mark every planned case below as passed.

## Four independent limit layers

Every case records each layer separately. Passing one layer says nothing about the next.

| Layer | What to record | Example |
|---|---|---|
| Format specification | Edition, profile/subformat, field constraints, valid dimensions and frame semantics. | A PNG width/height is at most 2^31−1; this is not a practical decode promise. |
| Decoder implementation | Exact library/version/build options, dimension/pixel/metadata limits, working buffers, cancellation and region cost. | A library may reject a valid file below its format limit. |
| Windows/GPU | WIC codec availability, API argument widths, D3D Feature Level, texture limits and actual allocation failures. | FL 11.x Texture2D side limit is 16384; WIC `CopyPixels` stride and buffer size are `UINT`. |
| Pixstride policy | CPU/GPU/disk budgets, concurrency, supported capability and expected downgrade/error. | A valid file can be rejected as over budget without being labelled corrupt. |

Sources: [PNG](https://www.w3.org/TR/png-3/),
[D3D11 limits](https://learn.microsoft.com/en-us/windows/desktop/direct3d11/overviews-direct3d-11-resources-limits),
[WIC CopyPixels](https://learn.microsoft.com/en-us/windows/win32/api/wincodec/nf-wincodec-iwicbitmapsource-copypixels).
Use checked 64-bit arithmetic before converting dimensions, strides, offsets or allocation lengths.

## Levels and outcomes

| Level | Scope | Required observation |
|---|---|---|
| L1 | Small valid files, color, orientation, Alpha, pages/animation where applicable. | Correct output against an independent reference and bounded metadata. |
| L2 | 8K/16K-class images within each format's actual limit, high compression ratios, large files and unusual aspect ratios. | Header routing, decode/convert/upload times, process and owned-memory peaks, responsiveness. |
| L3 | Extreme dimensions, huge decoded size, long animations/many pages, tile/grid layouts and disk-cache reuse. | Bounded resources, preview/region availability, cancellation, reopen and cleanup; controlled rejection is valid when capability is absent. |
| L4 | Truncation, corrupt fields/offsets/chunks, overflow, allocation failure, disk full and worker/device failure. | Structured error, no stale image selection or out-of-bounds access, recovered reservations and handles. |

8K/16K describes test classes, not a demand to encode an invalid dimension. For a format/library with a
lower limit, test just below, at, and just above that limit with the expected accept/reject outcome.
Resource faults apply at every level; L4 isolates them for reproducibility.

## Format matrix

| Format | L1 | L2 | L3 | L4 |
|---|---|---|---|---|
| JPEG | Baseline/progressive, grayscale, CMYK, EXIF orientation and ICC. | Large baseline/progressive, high/low entropy, native scaled output. | Near the selected decoder's size limit, tall scans, progressive coefficient memory and cancellation. | Truncated scans, invalid marker lengths/dimensions, hostile metadata and budget failure. |
| PNG | Palette/tRNS, grayscale, RGBA, 8/16-bit, Adam7. | 8K/16K, extreme compression, wide rows; compare interlaced/non-interlaced. | Private 48000 × 32000 sample; first build, Tile Pyramid, 100% regions and disk-cache hit. | CRC/truncated IDAT, invalid filter/size, ancillary expansion, disk-full/cancelled build. |
| GIF | Palette/transparency, offsets, delay, disposal and loop behavior. | Large canvas with small subframes; timing extremes. | Many frames, previous-disposal snapshots, bounded canvas/frame caches and time-based skipping. | Invalid LZW/rectangles, truncated sub-blocks, impossible extents and allocation failure. |
| TIFF | Strip/tile, multi-page, orientation, depth and supported compression. | Large strips/tiles, many IFD entries, classic TIFF and BigTIFF separately. | Synthetic BigTIFF beyond 4 GiB, huge tiled pages, many pages, actual region reads. | Offset overflow/cycles, invalid counts, overlapping/out-of-file regions and unsupported codecs. |
| WebP | VP8/VP8L, Alpha/ICC, animation blend/disposal. | Near coding/library limits, high compression, large animation canvas where supported. | Many frames, canvas/subframe mismatch, incremental decode memory and cancellation. | RIFF/chunk length errors, frame overrun, truncation and budget exhaustion. |
| AVIF | Supported profiles, chroma subsampling, high depth, Alpha/ICC/NCLX and sequences. | Large images with YUV planes and RGB conversion accounted separately. | Grid/layer/sequence cases supported by the pinned backend; reference-frame/worker memory. | Invalid boxes, dimensions/auxiliary mismatch, missing data, thread/memory exhaustion. |
| HEIC/HEIF | Supported HEVC profile, orientation, ICC/NCLX and auxiliary image handling. | Large coded image versus grid-derived image. | Many grid cells, visible-cell access if the adapter supports it, backend/thread limits. | Bad item references/grid extents, cycles, truncated HEVC, unsupported profile and exhaustion. |
| JPEG XL | Lossless/lossy, supported depth, orientation, ICC and animation. | Modular/VarDCT, optional progressive output and aspect-ratio extremes. | Supported level/dimension boundaries, many frames, callback output and decoder working memory. | Invalid codestream/boxes, inconsistent size, truncation, runner/memory limits. |
| RAW | Permitted camera fixtures or conformant DNG; embedded preview and full decode separately. | Sensor dimensions, CFA/bit depth and demosaic peak buffers. | Very large DNG/approved captures, preview fallback and bounded worker execution. | Vendor/container offsets, corrupt thumbnails, decompression limits and unsupported camera errors. |
| PSD/PSB | Composite preview, color mode, depth, Alpha and compression. | Large composite and many layers without promising layer editing. | PSB/huge declared canvases, selective composite access where supported; budget-based rejection. | Layer lengths/counts, recursive sections, RLE/ZIP truncation and malicious dimensions. |

## Format limits are not interchangeable

| Format | Specification boundary to distinguish | Implementation check before generation |
|---|---|---|
| JPEG | Common JPEG frame dimensions use 16-bit fields; other JPEG families/profiles require their own assessment. | libjpeg-turbo's current `JPEG_MAX_DIMENSION` is 65500, below the unsigned 16-bit maximum. Recheck the pinned tag and WIC separately. |
| PNG | Dimensions 1 through 2^31−1; IDAT data forms one zlib stream for the image. | Row/Adam7 buffers, pixel policy, metadata expansion and library limits; no random rectangle promise. |
| GIF | Logical canvas/image dimensions use 16-bit fields; frame/compositing semantics are separate. | Cap canvas bytes, decoded-frame storage and total work, not just compressed bytes. |
| TIFF | Classic TIFF uses 32-bit offsets; BigTIFF uses 64-bit offsets. This does not imply an identical pixel limit or universal BigTIFF support. | Record BigTIFF capability, tag types, strip/tile organization and decompressor working set. |
| WebP | VP8L represents dimensions up to 16384; VP8 and VP8X have different fields/constraints. The container size limit is 4 GiB minus 2 bytes. | Encoder and decoder limits can differ; inspect `WEBP_MAX_DIMENSION` and animation-canvas limits in the selected version. |
| AVIF | The AVIF profile, selected AV1 frame, `ispe`, grid and sequence constraints must agree. | Record backend dimension/pixel limits and plane/reference/RGB allocations; do not infer a maximum from one API integer. |
| HEIC | HEIF derived-image geometry and HEVC profile/level limits are separate. | Record actual WIC extension or libheif/backend versions; grid access is capability-specific. |
| JPEG XL | Size headers and codestream levels impose their own constraints. | Pin libjxl, runner, output callback behavior and decoder limits before declaring a maximum. |
| RAW | RAW is a family of camera/vendor formats, not one universal dimension specification. | Name the subformat and camera/DNG profile; distinguish metadata, preview, unpack and demosaic. |
| PSD/PSB | Adobe specifies up to 30000 per side for PSD and 300000 for PSB. | A composite decoder can implement a much smaller subset; measure layer/ZIP/RLE memory separately. |

Primary references: [libjpeg-turbo header](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/main/src/jmorecfg.h),
[GIF89a](https://www.w3.org/Graphics/GIF/spec-gif89a.txt),
[BigTIFF](https://libtiff.gitlab.io/libtiff/specification/bigtiff.html),
[WebP container](https://developers.google.com/speed/webp/docs/riff_container),
[VP8L](https://developers.google.com/speed/webp/docs/webp_lossless_bitstream_specification),
[AVIF](https://aomediacodec.github.io/av1-avif/),
[libheif tiled images](https://github.com/strukturag/libheif/wiki/Reading-and-Writing-Tiled-Images),
[libjxl](https://github.com/libjxl/libjxl), [LibRaw](https://www.libraw.org/about),
[Adobe formats](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/).
Unspecified maxima remain an explicit research item for the chosen adapter; test acceptance is not
inferred from the extension alone.

## Private extreme PNG protocol

The sample has 1,536,000,000 pixels. RGBA8 storage would require 6,144,000,000 bytes, about 5.72 GiB;
that is a calculation, not an allocation or measured peak. Keep its path out of public files.
The proposed local convention is `PIXSTRIDE_STRESS_IMAGE` for read-only input and `PIXSTRIDE_TEST_ROOT`
for a separate, cleanable cache/output root. No viewer or generator currently consumes these variables.

Before each run, check cloud-file residency without opening the data stream. Obtain the sample owner's
consent before a read that requires cloud download. Do not rename, modify, copy, upload or commit the
source. Mutation tests use separately generated fixtures.

Start with small oracle-checked PNGs. Then compare 512/1024/2048 Tiles under the same trajectory,
including first partial preview, full preview, first 100% region, arbitrary bottom-region availability,
verified build completion, cancellation at several progress points, and close/reopen.
The 1024 Tile and 512 MiB builder budget are experiment inputs. A non-interlaced first build may need
a full scan; a valid disk cache can serve independently stored Tiles on reopen. Do not merge those timings.

Record owned allocations, process private bytes/working set, OS cache, VRAM and disk usage separately.
Apply CPU/GPU/disk failures, corrupt only generated cache records, and verify source revision checks,
no obsolete result overwrites, reservation recovery, and handle release. Clean outputs only after
resolving their paths under the configured output root and confirming no active writer/reader lease.

## Independent fixture generator proposal

Develop a separate test utility after its own approval. It takes a declarative manifest, dimensions,
pattern/seed, format options, frame/page count and output root; it has no input-image option.
Generate pixel functions by rows/Tiles for flat fields, ramps, checkerboards, transparent edges and
deterministic noise. Bound producer buffers and encoded output; do not generate a giant full bitmap
to test bounded decoding. If the encoder cannot stream a requested case, report its limit or skip it.

Write a manifest with case ID, L1–L4 level, expected accept/downgrade/reject outcome, generator/backend
versions, seed, exact dimensions/strides/depth/frame dependencies, layer-specific limits, expected
pixel/ROI hashes, file checksum, size and permission to redistribute. Keep paths relative to the output
root. Use small licensed fixtures in Git only after review; large and private artifacts remain local.

Generate malformed cases from the utility's own fixtures by explicit mutations. Stop on disk/CPU/time
budget exhaustion and resume through case manifests, not hidden partially valid output.
Synthetic DNG does not validate all vendor RAW formats; authentic camera fixtures require provenance
and permission. PSD/PSB generators need an independent format validator. Encoder rejection is distinct
from decoder failure. Compare decoded pixels/regions with an independent implementation where feasible.

## Measurements and acceptance

Report cold process, OS-warm reads, CPU cache hits, GPU cache hits, disk Tile hits, and first builds
separately. Timestamp input receipt/publication, CPU decode/convert, upload submission, GPU completion,
`Present`, and Displayed. `Present` return and GPU API return do not prove pixels reached the screen.
Use [GPU timestamp disjoint checks](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ns-d3d11-d3d11_query_data_timestamp_disjoint)
and correlate presentation events; physical input-to-photon requires separate hardware measurement.

The initial targets remain P95 50 ms for warm-start window visibility, P95 5 ms for GPU-hit input-handler
start to snapshot publication, and P95 1 ms for ordinary input-handler work. They are unmeasured and
not displayed-frame latency promises. Collect at least 10,000 input events for tail analysis; label P99
as sample-insufficient below 1000 observations. For expensive first builds, report each run and range
instead of claiming stable P95/P99 from a few trials. Record failures and trace loss.

Every L3 capability needs correct preview/regions or an explicit unsupported/over-budget result,
responsive input, bounded queues/resources, cancellation/failure recovery and cleanup evidence.
Re-run the ordinary-image baseline after adding an extreme path.

Stage C executable row/cache/viewport probes are listed in [Stage C](stage-c.md). Small generated
fixtures run in Rust CI; explicit GPU and private-image probes remain opt-in. The actual extreme
image is never a CI input. Cache budgets are independent of the ordinary 256 MiB image limit.
