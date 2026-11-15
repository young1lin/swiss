# Cutting a release of swiss

Use when the user asks to publish, release or cut a version of swiss (发布 / 发版 / release a
version), or to push and tag one. Shipping the change to the local production instance on
19999 is [swiss-deploy](../swiss-deploy/SKILL.md) territory; this skill is the GitHub release.

The owner of the order is CI, not the agent (SPEC §release.ci): a `v*` tag runs every gate,
then the `release` job builds the five archives, the `.zst` files, the patches and
`update.json`, and creates the GitHub Release itself. **Never run `gh release create`** —
the tag is the whole instruction. The agent's job is the version, the changelogs, the tag and
the proof.

## Patch unless told otherwise

The default bump is **patch**. Choose minor or major only when the user explicitly asks for it
("发个 minor", "make this a major release"); a user who says nothing gets the patch. The tag
must equal `v` + `[workspace.package]` version — CI's `tag-version` job enforces the
equality — so the bump and the tag are one change, decided once.

## The cut, in order

1. **Summarize what changed** since the last tag:
   ```powershell
   git log --oneline (git describe --tags --abbrev=0)..HEAD
   ```
   then read `CHANGELOG.md`'s `[Unreleased]` section. That section IS the release summary the
   user asked for — cross-check the `feat()` commits against it: a merged round sometimes
   lands features its own changelog entries miss. Add the missing entries BEFORE cutting; a
   release that understates its round is wrong in both languages at once.
2. **Bump the version** in `[workspace.package]` of the root `Cargo.toml`, then refresh
   `Cargo.lock` (`cargo tree --workspace --depth 0` rewrites it and compiles nothing) and
   prove the pair with `cargo tree --locked --offline --workspace --depth 0`.
3. **Cut both changelogs the same way.** `CHANGELOG.md`: the `[Unreleased]` entries move under
   a new `## [X.Y.Z] - YYYY-MM-DD` (today, the operator's zone) and an empty
   `## [Unreleased]` header stays on top. `CHANGELOG_zh-CN.md` is the full Chinese twin
   (`## [未发布]` → `## [X.Y.Z] - <same date>`): same entries, same order, same date. When the
   Chinese side lags behind the English Unreleased, translate the backlog first, then cut —
   one release, two files, one commit. Follow the zh file's own punctuation habits
   (half-width commas and colons, full-width periods).
4. **README.md, when the round touches what it documents.** A new command
   (`swiss lane run`, `swiss update --apply`), a new page, a changed default: find the
   section that documents that area and update it in the same release commit. A README that
   still describes the old behavior ships with the release, it is not an optional extra.
5. **Commit as `release: X.Y.Z`** — `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`,
   `CHANGELOG_zh-CN.md`, plus the README when it changed. No `Co-Authored-By` trailer and no
   other author than young1lin (SPEC §release.hygiene authorship).
6. **Push, then tag.** `git push origin main`, then `git tag vX.Y.Z` on the release commit and
   `git push origin vX.Y.Z`. The tag push starts the workflow; concurrency rules let a tag's
   run always finish.
7. **Watch the workflow, then prove the release with gh.**
   ```powershell
   gh auth status                                  # before anything: logged in, repo scope
   gh run list --workflow=build.yml --limit 3      # find the run the tag started
   gh run watch <id> --exit-status                 # five targets; run it in the background
   gh release view vX.Y.Z                          # the proof
   ```
   Green CI is not the proof — `gh release view` is: the release exists, is not a draft and not
   a prerelease, and carries the assets the round promised — five archives, five `.zst` files,
   the `.patch.zst` set, `update.json`, `SHA256SUMS` and `SETUP.md`. A red gate means the
   release did not publish: say so, fix forward, delete the tag, re-tag.

## Honest reporting

The release is published when `gh release view` says so, not when the tag leaves the machine.
Name any gate that failed, name any asset that is missing, and never call a release done while
CI still runs.
