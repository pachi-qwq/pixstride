[English](README.md) | [简体中文](README.zh-CN.md)

<!-- doc-id: readme; revision: 3; status: design-review -->

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

## Read and contribute

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
