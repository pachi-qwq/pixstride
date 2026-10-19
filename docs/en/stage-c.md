[English](stage-c.md) | [简体中文](../zh-CN/stage-c.md)

<!-- doc-id: stage-c; revision: 1; status: implementation-review -->

# Stage C: large PNG path

Stages A and B are merged. Stage C is in local development; no large-image viewer support is claimed
until the cache and renderer gates pass. The private 48000 × 32000 PNG has not been decoded here.

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
