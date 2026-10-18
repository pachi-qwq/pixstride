[English](maintenance.md) | [简体中文](../zh-CN/maintenance.md)

<!-- doc-id: maintenance; revision: 2; status: proposal -->

# Repository maintenance and publication

This page defines the intended workflow and decisions requiring the owner's approval. Configuration
files in this checkout do not activate remote GitHub settings or publish anything.

## Empty-repository bootstrap

The 2026-10-18 read-only audit found a public remote with no branches and no commits; the commits API
returned HTTP 409. Repository metadata named `master` as the default, and the rulesets list was empty.
Recheck the remote before publication: a past empty result is not permission to overwrite new work.

The owner selected `MIT OR Apache-2.0` and `main`. Local `main` preserves the root commit `95f0d4f`
containing `.gitignore`, `.editorconfig`, and `.gitattributes`, then adds bilingual READMEs, both license
texts, and narrower ignore rules. An independent Worktree protects the existing untracked documents.
`docs/repository-foundation` fast-forwards to that baseline and adds the reviewed documentation/tools
as a separate commit. Original drafts and private samples remain outside Git history.

New commits use the account's GitHub noreply identity through repository-local configuration. The root
commit retains its original author metadata; global Git settings and existing history are unchanged.
Review both the tree and commit metadata before publication.

Remote publication still requires explicit approval after local checks and commits. Recheck refs and
stop if the remote has changed. Push `main` first, verify its README and licenses, and check whether
GitHub already selected it as the default. Change the default only if needed; inspect any real `master`
branch before acting. Then push the documentation branch and open its PR against `main`, without
merging it. The first PR includes the documentation workflow: verify that GitHub actually runs it
successfully before requiring its observed check name. If it is not scheduled, report the bootstrap
limitation and keep the check optional until resolved. Later changes use normal PRs.
This plan does not itself authorize a push, settings change, history rewrite, or branch deletion.

## Branches and pull requests

| Branch | Purpose |
|---|---|
| `main` | Reviewed, check-passing baseline; a buildable application once a Cargo project exists. |
| `feat/*` | New functionality. |
| `fix/*` | Bug fixes. |
| `docs/*` | Documentation. |
| `refactor/*` | Refactoring without intended feature changes. |
| `test/*` | Tests and benchmarks. |
| `chore/*` | Build and repository maintenance. |

Create short-lived branches from current `main`, run the relevant checks, open a PR, review behavior,
correctness and performance impact, squash merge, then delete the merged feature branch.
Do not create a long-lived `develop`. Conventional Commits are recommended without extra hooks.
Require a PR but allow zero required approvals so a solo maintainer can review and merge their own work.

## Remote settings awaiting approval

| Setting | Recommendation | When to apply |
|---|---|---|
| Default branch | Use `main`; change it only if GitHub has not already selected it. | After the first approved `main` push and a fresh branch check. |
| PR requirement | Require PRs for `main`; required approvals 0; do not require a different reviewer or code-owner approval. | After bootstrap. |
| Status checks | Require the successful documentation job `docs`; verify the actual context after its first run. | After CI has reported it; no path filters that leave required checks pending. |
| History | Block force pushes and deletion of `main`; consider linear history with squash merging enabled. | After bootstrap, with a reviewed bypass policy. |
| Merge methods | Prefer squash; the initial remote allowed squash, merge and rebase. | Owner decides whether to disable other methods. |
| Feature branches | Enable automatic deletion after merge; initial value was false. | Owner-approved setting; branch deletion remains a remote action. |
| Visibility | Keep the existing public visibility. | No change is proposed. |

Rulesets can require PRs without approvals, block force pushes/deletions, and require status checks.
Feature/plan availability and bypass settings must be verified when applying them.
[GitHub rules documentation](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
is the reference. Do not enable a required check before it exists or impose an approval rule the solo
maintainer cannot satisfy. Agents must obtain explicit authorization for each remote action in scope.
Automatic deletion does not guarantee deletion of a contributor's fork branch: the fork's setting and
the merger's write access also matter. See
[GitHub's branch-deletion rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-the-automatic-deletion-of-branches).

## Documentation CI

The local workflow is named `Documentation`, with a Windows job named `docs`. It uses Python's standard
library, read-only repository permissions, commit-pinned official checkout/setup actions, and no secrets.
Push and PR events run local checks and checker tests without path filters. A manual dispatch can
add external-link checks; no workflow has been pushed or run remotely in this task.

Hard failures cover Markdown structure/local links, language entries/revisions/shared literals, known
private-path/credential patterns, and oversized/publicly disallowed artifacts. Language-quality findings
are advisory. The checker tests guard against false positives in code, API names and link syntax.
It is a focused checker, not a full GFM implementation, secret-scanning service, or translation verifier.

Future Rust CI runs on Windows and adds fmt, Clippy, tests and Release build only when Cargo exists.
Hardware latency/VRAM tests run separately. Keep Actions updates reviewable; no auto-merge or release
permissions are needed for this documentation workflow.

## Publication review

The original ten Markdown files were backed up and archived outside the repository with SHA256
verification. Detailed early designs and machine logs remain internal. The public architecture page is
a scoped review summary; all original exception paths and design history remain available privately.

Before any push, inspect tracked files and the staged diff, not just ignored files. Inspect reachable
history if any commits exist. `.gitignore` does not remove tracked files or old blobs. If a private file
was tracked, prepare a local report identifying commits and paths without echoing secrets, stop
publication, rotate any exposed credentials through the owner, and propose remediation for approval.
Do not automatically force-push, rewrite history or delete remote content.

Keep local paths and machine identity out of public files. Proposed local sample/output configuration
is shown in [`.env.example`](../../.env.example); real `.env` files stay ignored. Large test data,
generated caches and reports belong outside Git. New small licensed fixtures require separate review.
Use `assets/` for project images and `tests/fixtures/` for small reviewed test images. These paths are
not ignored by extension. The documentation checker permits images up to 1 MiB there, without decoding
or inspecting embedded metadata; provenance, privacy, and license review remain manual obligations.
Private and generated data directories remain ignored; tracked files are still checked.

## License decision

Pixstride is dual-licensed under the MIT or Apache License, Version 2.0, at your option.
The owner approved the SPDX expression `MIT OR Apache-2.0`. Keep [LICENSE-MIT](../../LICENSE-MIT), with
`Copyright (c) 2026 pachi-qwq`, and the complete [LICENSE-APACHE](../../LICENSE-APACHE).
The MIT body was checked against the [official MIT text](https://opensource.org/license/mit);
Apache's full [official text](https://www.apache.org/licenses/LICENSE-2.0) is preserved unchanged.
Future Cargo packages use `license = "MIT OR Apache-2.0"`; no placeholder Cargo project is needed.

Check licenses before introducing dependencies or reusing code. Preserve required third-party copyright
and license notices verbatim and identify external sources. Do not present external code as Pixstride's
original work. Pixstride's license does not replace third-party obligations or approve every future
distribution choice. The writing references retain their own licenses.

Potential codec impact must be reviewed against exact versions and build options:

- WIC uses installed Windows codecs; confirm extension availability and redistribution terms rather
  than shipping system DLLs.
- libjpeg-turbo has IJG/BSD and component notices; libpng/zlib and libwebp have their own notice terms.
  libavif/libjxl and selected backends also require license/IP-notice review.
- libheif is LGPL; static versus dynamic distribution and each codec backend need an explicit plan.
  LibRaw offers LGPL-2.1/CDDL-1.0 options; extra packs/dependencies can change the obligations.
- Build-only tools, bundled fixtures, fonts and copied documentation also need provenance. Produce an
  SBOM and complete notices; a Rust crate's top-level license alone is insufficient.

References: [libjpeg-turbo licenses](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/main/LICENSE.md),
[libheif COPYING](https://github.com/strukturag/libheif/blob/master/COPYING),
[LibRaw licensing](https://www.libraw.org/about),
[libjxl](https://github.com/libjxl/libjxl), [libavif](https://github.com/AOMediaCodec/libavif).
These are engineering review items, not a claim that every future linkage/distribution choice is approved.

## Versions and releases

Use `0.x.y` for early application releases once an application exists, with changelogs distinguishing
features, fixes, known limits and measured regressions. Version cache/config schemas separately.
Create annotated `vX.Y.Z` tags only for reviewed builds. Start with an x64 portable ZIP containing
the executable, selected runtime components, chosen license texts, notices and checksums.
Verify CRT dependencies on clean Windows machines before claiming a standalone executable.

Plan later automation to build/test on Windows, retain symbols privately/as appropriate, produce
checksums/SBOM/notices, and upload a draft release for owner review. Installation, signing, associations,
update policy and release publishing require separate decisions. No application-release workflow is
implemented in this documentation task.

Owner decisions still pending: first `main` push/default switch, protection/bypass policy, required
checks, merge options, branch deletion, and later release/signing policy. Local review can finish
before any of those remote actions occur.
