[English](AGENTS.md) | [简体中文](AGENTS.zh-CN.md)

<!-- doc-id: agents; revision: 3; status: active -->

# Agent instructions

## Current scope

Pixstride is under active development. Stay within the scope approved for the current task; Stage A
does not authorize image decoding, navigation, caching, or later-stage features. Before each task,
inspect Git status, the current branch, history, and upstream. Preserve untracked work before
switching branches or reorganizing files. Do not develop features directly on `main`.

## Rust source comments

Write Rust comments and Rustdoc in natural Simplified Chinese. Keep Rust identifiers, API names, and
established English technical terms unchanged. Use `//!` for module responsibilities, `///` for
public APIs and important interfaces, and `//` for implementation intent, constraints, and lifecycle;
do not narrate obvious statements. Every `unsafe` block must have an adjacent `// SAFETY:` comment
that names the concrete pointer, handle, thread, lifetime, or buffer preconditions and how the code
maintains them. Do not use `unsafe` merely to silence Clippy. Explain ownership, borrowing, `Result`,
`Option`, `Arc`, `Mutex`, channels, `Send`/`Sync`, COM, and D3D11 lifetimes when the code first relies
on a non-obvious constraint.

## Before editing public documents

Read [STYLE_GUIDE](docs/STYLE_GUIDE.md), [TERMINOLOGY](docs/TERMINOLOGY.md), and the matching language
version. Public documents default to English and have a complete Simplified Chinese counterpart.
Update both files, their shared revision, and [the pair catalog](docs/document-pairs.json) together.

Preserve API names, thread rules, resource ownership, exception paths, and measurement boundaries.
Label facts, proposals, estimates, and measured results accurately. A target is never a result.
Use the glossary for project concepts; explain owner and lifetime when adding a concept.
Do not silently change approved architecture decisions. Propose the change with evidence and update
the corresponding design documents when an important architecture change is approved.

## Privacy and artifacts

Back up original design material outside the repository before reorganizing it. Do not delete it.
Keep internal drafts, machine reports, private samples, large fixtures, caches, and build outputs outside
the repository. Do not publish local absolute paths, credentials, or unredacted logs.
Use `PIXSTRIDE_STRESS_IMAGE` and `PIXSTRIDE_TEST_ROOT` for future local test inputs and outputs;
these are proposed test-tool conventions, not implemented viewer options.

## Checks and Git

Run `python -B scripts/check_docs.py` and `python -B -m unittest discover -s scripts/tests` after
documentation-tool changes; run the document check after prose changes. Review both languages manually.
Network link checks are opt-in: `python -B scripts/check_docs.py --external-links`.
When Rust work is approved, run the applicable fmt, Clippy, test, and build checks after changing Rust
code. Performance changes must describe a validation method and distinguish targets from results.

Use short-lived `feat/*`, `fix/*`, `docs/*`, `refactor/*`, `test/*`, or `chore/*` branches from `main`.
There is no long-lived `develop` branch. Follow checks → maintainer review → squash merge; agents
must not merge a PR automatically. See [maintenance](docs/en/maintenance.md) for first publication.
Do not push, change GitHub settings or visibility, rewrite history, or delete remote branches without
the owner's explicit authorization. Do not force-push or rewrite history as routine task cleanup.
Do not install administrator-only tools without approval.

## Licensing

The owner selected `MIT OR Apache-2.0`; keep [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE). Future Cargo packages use `license = "MIT OR Apache-2.0"`.
Check dependency and reused-code licenses before adoption, preserve required notices verbatim, and
attribute external work. Pixstride's license does not replace third-party obligations.
