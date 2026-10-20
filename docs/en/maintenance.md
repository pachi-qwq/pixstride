[English](maintenance.md) | [简体中文](../zh-CN/maintenance.md)

<!-- doc-id: maintenance; revision: 5; status: active -->

# Repository maintenance and publication

This page retains the 2026-10-18 repository-initialization record and describes the current workflow.
Stages A–C are merged; release work remains planned. Local configuration changes do not automatically change GitHub.

## Empty-repository bootstrap

Before the first push, the 2026-10-18 read-only audit found no remote branches or commits; the commits API
returned HTTP 409. Repository metadata named `master` as the default, and the rulesets list was empty.
Recheck the remote before publication: a past empty result is not permission to overwrite new work.

The owner selected `MIT OR Apache-2.0` and `main`. Local `main` preserves the root commit `95f0d4f`
containing `.gitignore`, `.editorconfig`, and `.gitattributes`, then adds bilingual READMEs, both license
texts, and narrower ignore rules. An independent Worktree protected the existing untracked documents.
`docs/repository-foundation` fast-forwarded to that baseline and added the reviewed documentation/tools
as a separate commit. Original drafts and private samples remain outside Git history.

New commits use the account's GitHub noreply identity through repository-local configuration. The root
commit retains its original author metadata; global Git settings and existing history are unchanged.
Review both the tree and commit metadata before publication.

After explicit owner approval, both branches were pushed normally. GitHub automatically selected `main`
as the default; its bilingual READMEs and both license texts were verified remotely.
[PR #1](https://github.com/pachi-qwq/pixstride/pull/1) was opened from `docs/repository-foundation` against
`main` and was subsequently merged by Squash Merge on 2026-10-18. The merge commit on `main` is
`2a1eb5254c01bb5b6fe4d61117ffa382755bc9b8`; GitHub deleted the remote documentation branch after merge.
The first documentation CI runs succeeded for both
[push](https://github.com/pachi-qwq/pixstride/actions/runs/39137217067) and
[pull request](https://github.com/pachi-qwq/pixstride/actions/runs/39137226043).
The observed `docs` check is required, `main` protection is active, and `main` remains the default branch.
The post-merge documentation run [39139503844](https://github.com/pachi-qwq/pixstride/actions/runs/39139503844) succeeded.
No history was rewritten, no force push was used, and automatic merging remains disabled.
Application work subsequently merged through [Stage A PR #2](https://github.com/pachi-qwq/pixstride/pull/2),
[Stage B PR #3](https://github.com/pachi-qwq/pixstride/pull/3), and
[Stage C PR #4](https://github.com/pachi-qwq/pixstride/pull/4).
Later changes use normal PRs; agents still need explicit authorization for remote actions.

## Branches and pull requests

| Branch | Purpose |
|---|---|
| `main` | Protected, reviewed baseline containing merged Stages A–C. |
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

## Current remote settings

| Setting | Verified state | Scope |
|---|---|---|
| Default branch | `main`, selected automatically after its first push. | Repository default. |
| PR requirement | PR required; required approvals 0; code-owner and last-push approvals disabled. | `main`. |
| Status checks | `docs` required, bound to GitHub Actions integration 15368; strict up-to-date policy enabled. | `main`; selected after actual successful CI runs. |
| History | Force pushes and branch deletion blocked; bypass list empty. | `main`. |
| Merge methods | Squash enabled and recommended; merge and rebase remain allowed. | Repository merge settings. |
| Feature branches | Automatic deletion after merge enabled. | Merged feature branches; the remote `docs/repository-foundation` branch was deleted after PR #1 merged. |
| Visibility and auto-merge | Repository public; automatic merging disabled. | No visibility change or automatic merge was performed. |

[The active `main-protection` ruleset](https://github.com/pachi-qwq/pixstride/rules/25346431)
applies to `refs/heads/main` with no bypass actors. Required approvals are zero so a solo maintainer can
review and merge through a PR. CI must pass before merging; the agent does not merge automatically.
[GitHub rules documentation](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
describes the available rules. Future remote changes still require the owner's explicit authorization.
Automatic deletion does not guarantee deletion of a contributor's fork branch: the fork's setting and
the merger's write access also matter. See
[GitHub's branch-deletion rules](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-the-automatic-deletion-of-branches).

## Documentation CI

The workflow is named `Documentation`, with a Windows job named `docs`. It uses Python's standard
library, read-only repository permissions, commit-pinned official checkout/setup actions, and no secrets.
Push and PR events run local checks and checker tests without path filters. A manual dispatch can
add external-link checks. The first successful Windows runs checked 33 public files and passed all
41 checker tests. The workflow was merged to `main` with PR #1, and its post-merge run succeeded.
Subsequent PR updates must pass the required `docs` check again.

Hard failures cover Markdown structure/local links, language entries/revisions/shared literals, known
private-path/credential patterns, and oversized/publicly disallowed artifacts. Language-quality findings
are advisory. The checker tests guard against false positives in code, API names and link syntax.
It is a focused checker, not a full GFM implementation, secret-scanning service, or translation verifier.

A merged Windows Rust CI workflow runs fmt, Clippy, tests, Release build, and rustdoc for the Cargo
package. Its `rust` job and the `docs` job passed on the Stage C main merge:
[Rust](https://github.com/pachi-qwq/pixstride/actions/runs/39442403113) and
[Documentation](https://github.com/pachi-qwq/pixstride/actions/runs/39442402998). The current Ruleset
requires `docs`; `rust` is not configured as required. Hardware latency/VRAM tests run separately. Keep Actions updates reviewable;
no auto-merge or release permissions are needed for these workflows.

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
The existing Cargo package declares `license = "MIT OR Apache-2.0"`.

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

Use `0.x.y` for future early public releases, with changelogs distinguishing
features, fixes, known limits and measured regressions. Version cache/config schemas separately.
Create annotated `vX.Y.Z` tags only for reviewed builds. Start with an x64 portable ZIP containing
the executable, selected runtime components, chosen license texts, notices and checksums.
Verify CRT dependencies on clean Windows machines before claiming a standalone executable.

Plan later automation to build/test on Windows, retain symbols privately/as appropriate, produce
checksums/SBOM/notices, and upload a draft release for owner review. Installation, signing, associations,
update policy and release publishing require separate decisions. No application-release workflow is
implemented in this documentation task.

Repository initialization, protection, and the Stage A–C merges are complete. Stage D remains the next
planned phase; release and signing policies require separate approval. No public Release has been published.
