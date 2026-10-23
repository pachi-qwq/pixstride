[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 16; status: fullscreen-review -->

# Pixstride

A native Windows image viewer project focused on fast startup, responsive input, and bounded resource use.

Inspired by MassiGra's lightweight interface and fast interaction, Pixstride has completed and merged
Stages A–C. [Stage C PR #4](https://github.com/pachi-qwq/pixstride/pull/4) includes the owner's six passed
Windows manual acceptance scenarios. There is no public release. Stage D has started with technical
validation and merged EXIF correction, GIF playback and shared view geometry. Further scope requires approval.

## Current foundation

The selected stack is Rust Stable, Win32 through windows-rs, Direct3D 11, and a DXGI Flip Model
Swap Chain. The target is Windows 10/11 x64. The merged implementation includes:

- Native Win32 UI and D3D11 hardware rendering.
- JPEG/PNG/BMP browsing with background WIC decoding, directory navigation, CPU/GPU caches, adjacent
  preloading, and a GPU-cache-hit fast path.
- Large-PNG row streaming, a disk Tile Pyramid, viewport Tile rendering, zoom, and pan.
- Generation-based stale-work protection and bounded CPU/GPU cache residency with separate large-image budgets.

[D1-a GIF posters](docs/en/stage-d1a.md) are merged through
[PR #7](https://github.com/pachi-qwq/pixstride/pull/7): bounded structural validation and a composed
first image on a transparent canvas. All five owner-run Windows manual checks passed.
The [D1-b1 background animation worker](docs/en/stage-d1b1.md) is merged.
[D1-b2 D3D11 GIF playback](docs/en/stage-d1b2.md) is merged through PR #9, including bounded frame
handoff, Delay/Loop scheduling and Space pause/resume. The owner reports all eight Windows manual
acceptance scenarios passed for D1-b2.

[D3-a1 shared view geometry](docs/en/stage-d3a1.md) is merged through PR #10: cursor-anchored wheel zoom,
left-drag pan, and consistent Fit/100% for ordinary images, GIF frames and Tiles.
The project owner reports all 10/10 D3-a1 Windows manual checks passed.
This local branch adds [D3-a2 borderless fullscreen](docs/en/stage-d3a2.md), using F11/Esc and native
window restoration. Automated verification passes; owner desktop acceptance, PR review and merge remain pending.

The architecture keeps GPU resource ownership on a render thread and decodes images in
bounded background work. Large images have a separate path. Threading, cache budgets, and Tile sizes
remain subject to experiments; format names in the test plan are not a support claim.

First full large-PNG cache construction is slow and has no progressive preview. Historical viewport
tails are not fully attributed. Device Lost, actual disk exhaustion, allocation failure, and
long-duration stability still need broader testing. Some PNG encodings and ICC/HDR are unsupported;
see [Stage C limits](docs/en/stage-c.md). Present return does not mean a frame has reached the screen.

## Read and contribute

- [Stage A implementation and verification](docs/en/stage-a.md)
- [Stage B image browsing implementation and limits](docs/en/stage-b.md)
- [Stage C large PNG implementation and verification](docs/en/stage-c.md)
- [Architecture scope and review actions](docs/en/architecture.md)
- [Development status and documentation checks](docs/en/development.md)
- [Cross-format stress test plan](docs/en/stress-testing.md)
- [Writing style](docs/STYLE_GUIDE.md) and [terminology](docs/TERMINOLOGY.md)
- [Contributing](CONTRIBUTING.md)
- [GitHub workflow, licensing, and release plan](docs/en/maintenance.md)

Documentation tooling uses Python 3.11 or later and the standard library:

```sh
python -B scripts/check_docs.py
python -B -m unittest discover -s scripts/tests
```

Keep private samples, draft designs, machine reports, and generated caches outside the repository.
Use local configuration or environment variables for test paths. Do not attach private images or
unredacted diagnostics to issues.

## License

Pixstride is dual-licensed under the MIT or Apache License, Version 2.0, at your option.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
The SPDX license expression is `MIT OR Apache-2.0`.

Third-party components retain their own licenses and notice requirements.
