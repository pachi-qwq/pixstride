[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 2; status: design-review -->

# Pixstride

A native Windows image viewer project focused on fast startup, responsive input, and bounded resource use.

Inspired by MassiGra's lightweight interface and fast interaction, Pixstride is in
Design Review / Repository Foundation. This repository contains documentation and repository tooling.
There is no viewer application, Cargo project, downloadable build, or measured product performance yet.

## Planned foundation

The selected stack is Rust Stable, Win32 through windows-rs, Direct3D 11, and a DXGI Flip Model
Swap Chain. The target is Windows 10/11 x64. WIC is the first decoder to evaluate for ordinary images.

The proposed architecture keeps GPU resource ownership on a render thread and decodes images in
bounded background work. Large images have a separate path. Threading, cache budgets, and Tile sizes
remain subject to experiments; format names in the test plan are not a support claim.

## Documentation and contributions

Architecture summaries, contributor guidelines, and documentation checks are being prepared on a
separate documentation branch for review. Viewer implementation requires a separately approved task.

Keep private images, internal drafts, machine reports, large test data, and generated caches out of
Git. Small licensed project assets and reviewed fixtures may be committed in `assets/` and
`tests/fixtures/`.

## License

Pixstride is dual-licensed under the MIT or Apache License, Version 2.0, at your option.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
The SPDX license expression is `MIT OR Apache-2.0`.

Third-party components retain their own licenses and notice requirements.
