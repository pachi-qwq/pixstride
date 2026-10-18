[English](development.md) | [简体中文](../zh-CN/development.md)

<!-- doc-id: development; revision: 2; status: design-review -->

# Development status and checks

The repository currently contains public documentation, templates, and Python documentation tools.
It has no Cargo workspace or viewer binary. Start with the document checks; Rust build commands become
applicable when stage A implementation is separately approved.

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

## Prepare Windows development later

Stage A will need Rust Stable with `x86_64-pc-windows-msvc`, Cargo, rustfmt, Clippy, MSVC C++ build tools,
and a compatible Windows SDK. Check existing versions before installing or changing anything.
Use a project-local toolchain selection after review; do not change another project's default toolchain.

A prior local probe used Rust/Cargo 1.96.0 and windows-rs `windows` 0.62.2, compiled through MSVC,
and created a WIC Factory and D3D11 hardware device. This demonstrates that combination on one machine,
not a project-wide pin, clean-machine reproduction, or performance result.

The probe's debug-device creation returned `0x887A002D` (missing SDK debug component). Graphics Tools
installation may require administrator permission and must be approved. PresentMon 2.6.0 CLI help,
official digest and signature were verified locally; live capture was not. WPR profile discovery worked;
WPA/trace analysis and Swap Chain/Shader/WARP behavior still need validation. Raw logs remain private.

Use [Rust's Windows MSVC guidance](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html),
[windows-rs](https://github.com/microsoft/windows-rs),
[Microsoft Graphics Tools guidance](https://learn.microsoft.com/en-us/windows/ai/directml/dml-debug-layer),
[WPA](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/windows-performance-analyzer), and
[PresentMon](https://github.com/GameTechDev/PresentMon) as installation/research sources.

## When a Cargo project is approved

Add a Windows Runner job for `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --locked`, and `cargo build --release --locked`. Resolve/pin dependencies first and check the
actual selected features. Do not add a placeholder viewer just to make CI run.
Shared CI verifies correctness/builds; dedicated hardware measures latency and resource peaks.
See [stage A scope](architecture.md) and [maintenance](maintenance.md) before publishing changes.
