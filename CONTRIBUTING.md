[English](CONTRIBUTING.md) | [简体中文](CONTRIBUTING.zh-CN.md)

<!-- doc-id: contributing; revision: 2; status: active -->

# Contributing

Pixstride is in design review. Contributions currently cover documentation, review proposals,
repository configuration, CI, and documentation tooling. Start viewer implementation only after a
separate approved task.

## Prepare a change

Read the [style guide](docs/STYLE_GUIDE.md), [terminology](docs/TERMINOLOGY.md), and
[development guide](docs/en/development.md). Describe the problem and the intended result before
adding a dependency or extending a design. Preserve the distinction between API constraints,
estimates, proposals, and measured behavior.

After the initial `main` branch is published, create a short-lived branch from current `main`:

```sh
git switch main
git pull --ff-only
git switch -c docs/describe-change
```

Use `feat/*`, `fix/*`, `docs/*`, `refactor/*`, `test/*`, or `chore/*` as appropriate.
There is no long-lived `develop` branch. The empty-repository bootstrap is documented in
[maintenance](docs/en/maintenance.md); the commands above do not create the missing initial history.

## Document and check

Update English and Simplified Chinese pages together, with the same document ID and revision in
[the catalog](docs/document-pairs.json). Templates may use English and Chinese in one file.
New terms need a definition, owner and lifetime. API names and units keep their exact spelling.

```sh
python -B scripts/check_docs.py
python -B -m unittest discover -s scripts/tests
```

Run the second command when changing documentation tools; it tests the checker, not a viewer.
Optionally check remote links with `python -B scripts/check_docs.py --external-links`.
Review advisory findings, translations, API facts, and readability manually. A passing script is not
evidence that the implementation meets a performance target.

## Open a pull request

Use a focused PR that explains the problem, final behavior, evidence, and remaining risks. Use the
provided template and report what ran, what did not, and why. Conventional Commits are recommended,
for example `docs: define stage A ownership`; no commit-hook framework is required.

The normal flow is checks → maintainer review → squash merge → delete the merged feature branch.
A solo maintainer can self-review and merge through a PR; required external approvals should remain
zero. Do not force-push shared history or change remote settings as part of a routine contribution.
Agents need the owner's explicit authorization before any push or remote-setting change.

Keep private images, generated large files, machine reports, local absolute paths, and credentials out
of commits and issue attachments. Ignoring a tracked file does not remove it from history: stop before
publishing, inspect exposure, and obtain a specific remediation decision. Do not rewrite history
automatically. See [publication checks](docs/en/maintenance.md#publication-review).

## Licensing and resources

Pixstride is dual-licensed under the MIT or Apache License, Version 2.0, at your option.
See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE); the SPDX expression is
`MIT OR Apache-2.0`. Unless explicitly stated otherwise, intentional contributions are submitted under
these same terms. Future Cargo packages use `license = "MIT OR Apache-2.0"`.

Before adding a dependency or reusing code, verify its license and preserve required third-party
copyright and license notices verbatim. Attribute external work; do not present it as original Pixstride
code. Pixstride's license does not override third-party obligations.

Small licensed images may live in `assets/` or `tests/fixtures/` after review of provenance, metadata,
privacy, and size. The checker allows images up to 1 MiB in those directories without decoding them;
it cannot approve their contents or license. Keep private, generated-large, and stress data outside Git.
