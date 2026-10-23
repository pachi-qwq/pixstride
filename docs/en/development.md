[English](development.md) | [简体中文](../zh-CN/development.md)

<!-- doc-id: development; revision: 18; status: view-transform-review -->

# Development status and checks

The repository contains a single Cargo package for a Win32/D3D11 viewer. Merged Stage B provides WIC decoding for
JPEG, PNG, and BMP, directory navigation, and bounded CPU/GPU caches. The project owner has confirmed
real-image display, zoom, keyboard navigation, the native file dialog, and window resizing.
Stages A–C are merged. Stage C provides separately budgeted PNG streaming, disk Tile Pyramid construction,
and viewport rendering with zoom/pan. Its six owner-run Windows manual acceptance scenarios passed.
See [Stage C](stage-c.md) for cache variables, build measurements, supported encodings, and manual checks.
There is no public release. Stage D has merged EXIF 1–8 correction, static GIF posters and
[D1-b2 GIF playback](stage-d1b2.md), including all eight owner-reported Windows manual scenarios.
This branch adds [D3-a1 shared view geometry](stage-d3a1.md). The project owner reports all ten
Windows manual checks passed; PR review and merge remain pending.
Further Stage D scope remains subject to approval.

## Build and run

Use Windows 10/11 x64, Rust Stable with `x86_64-pc-windows-msvc`, MSVC C++ Build Tools, the Windows SDK,
and `fxc.exe`. The build script compiles HLSL into DXBC in Cargo's `OUT_DIR`; set `PIXSTRIDE_FXC` to
the compiler's full path if SDK discovery fails. No shader compiler runs during normal application use.

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
cargo doc --no-deps --locked
cargo run --release
```

With no argument, the program displays the Stage A generated test textures. Pass one image path to open
JPEG, PNG, BMP, or GIF (multi-frame playback, Space to pause/resume); `Ctrl+O` opens the native file dialog, Left/Right navigate the current directory,
`F` selects Fit, and `1` selects 1:1 physical pixels. On this branch, ordinary images, GIF and eligible
large-PNG Tiles share cursor-anchored wheel zoom and left-drag pan. New images retain the mode and
reset extra Zoom/Pan; GIF frame changes retain the view. Resize keeps the image point under the client center. On exit, stdout prints bounded timing samples and cache
counters. The Release executable is `target/release/pixstride.exe`. Feature details and limits are in
[Stage B](stage-b.md) and [Stage C](stage-c.md).

## D3-a1 view regression

From the repository root, use a dedicated directory outside Git for generated fixtures and Tile data.
These probes use synthetic inputs and do not open private stress images:

```powershell
$env:PIXSTRIDE_TEST_ROOT = [IO.Path]::GetFullPath('..\..\logs\pixstride-d3a1\samples')
$env:PIXSTRIDE_TILE_CACHE = [IO.Path]::GetFullPath('..\..\logs\pixstride-d3a1\tile-cache')
cargo test --release --locked release_view_transform_gpu_pixels -- --ignored --nocapture
cargo test --release --locked release_view_transform_input_and_playback -- --ignored --nocapture
cargo test --release --locked release_frame_admission_survives_stale_selection -- --ignored --nocapture
cargo test --release --locked release_animation_prepare -- --ignored --nocapture
cargo test --release --locked release_prepare_gif_poster_samples -- --ignored --nocapture
cargo test --release --locked release_prepare_gui_samples -- --ignored --nocapture
```

The geometry probe reports `pixels_checked=7869`; the input probe reports 120 transform steps with
no additional decode/upload. Generators print their retained output directories. For manual checks,
open `animation-fixtures/partial.gif`, the poster directory's JPEG/PNG/BMP and `wide.png` under the
printed large-image sample directory. Use Ctrl+O to switch between those directories. Keep the dedicated
Tile cache; never point these probes at an existing private cache. Acceptance criteria are in
[D3-a1](stage-d3a1.md#manual-acceptance-and-remaining-limits).

## Run the current checks

Use Git and Python 3.11 or later. No Python package installation is required. From the repository root:

```sh
python -B scripts/check_docs.py
python -B -m unittest discover -s scripts/tests
```

The checker examines tracked and non-ignored candidate files, including tracked files that later became
ignored. It checks Markdown structure, local links/fragments, bilingual entries/revisions/shared literals,
file size and known private-path/credential patterns. Diagnostics do not echo matched secrets.
Images up to 1 MiB may be included in `assets/` and `tests/fixtures/` after manual provenance, privacy,
and license review. The checker bounds their file size without decoding or scanning binary metadata;
images elsewhere, private data, generated-large files, and caches are not public inputs.

Prose checks understand fenced/indented code, inline code, link destinations, API identifiers, quoted
rewrite examples, and HTML comments. Chinese spacing/punctuation, suspicious terminology spelling and
selected filler phrases are advisory. Review the sentence instead of replacing words mechanically.
Privacy checks still inspect code and comments. This is a documented GFM subset, not a complete parser
or secret scanner; unusual constructs need manual review.

External links are opt-in:

```sh
python -B scripts/check_docs.py --external-links
```

The network check validates HTTP(S) reachability, not external page fragments or technical claims.
Definite 404/410 responses fail; authentication/rate limits, timeouts and blocked requests need review.
Use normal CI for deterministic local checks; run the optional network step manually when useful.

Tests use temporary files only. Set `PIXSTRIDE_DOC_TEST_ROOT` to an existing repository-external folder
if the platform's default temporary directory is unsuitable. Keep reports outside the repository.
The `-B` option avoids Python bytecode output in the working tree.

## Verified local toolchain and limits

The current validation environment has Rust/Cargo 1.96.0, target `x86_64-pc-windows-msvc`, Visual Studio
Build Tools 2022 with MSVC 14.44, and Windows SDK 10.0.26100.0. The project pins `windows` 0.62.2 and
uses `anyhow` 1.0.104. `Cargo.lock` records the resolved dependency graph. The Release build compiled
both HLSL entries with the SDK's FXC.

A project-owner manual Release run on an NVIDIA GeForce RTX 4080 SUPER using the Hardware Driver at
Feature Level 11.1 confirmed the checkerboard, Left/Right texture switching, maximize/minimize/restore,
repeated resize, and Alt+F4 shutdown. The owner recorded D3D11 initialization at 183.236 ms and device
creation at 177.742 ms. See [Stage A verification](stage-a.md) for the full timings and their limits.
Controlled cold/warm startup, Input-to-Photon, Device Lost stress, and long-duration stability remain
unverified. Compilation alone does not establish GUI behavior.

The earlier environment probe's debug-device creation returned `0x887A002D` (missing SDK debug
component). The current application does not request the D3D11 debug layer. Installing Graphics Tools
may require administrator permission and needs separate approval. PresentMon 2.6.0's CLI help, official
digest, and signature were previously verified locally; live capture and WPA trace analysis remain
unverified. Raw probe logs stay outside the repository.

Use [Rust's Windows MSVC guidance](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html),
[windows-rs](https://github.com/microsoft/windows-rs),
[Microsoft Graphics Tools guidance](https://learn.microsoft.com/en-us/windows/ai/directml/dml-debug-layer),
[WPA](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/windows-performance-analyzer), and
[PresentMon](https://github.com/GameTechDev/PresentMon) as installation/research sources.

## CI and performance validation

The `Rust` workflow uses a Windows runner and a `rust` job. It runs formatting, Clippy, tests, Release
build, and Rustdoc with the checked-in lockfile. The workflow has read-only repository permissions and
does not publish releases. This task did not change required status checks or repository protection.

Application timings use `std::time::Instant` and retain at most 4096 recent samples per category.
Percentiles use nearest rank. Startup ends at `CreateWindowExW` return, not first display. `Present`
duration is not display latency. Use PresentMon/ETW for displayed-frame events and an external method
for Input-to-Photon measurement. See [Stage A verification](stage-a.md), [Stage B](stage-b.md), [Stage C](stage-c.md), [architecture](architecture.md),
and [maintenance](maintenance.md) for the current implementation and limits.
