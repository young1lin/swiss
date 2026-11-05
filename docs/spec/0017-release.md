# §release — CI, releases and repository hygiene

Chapter 0017 of the [swiss specification](../SPEC.md).

swiss ships as one binary per platform from GitHub Releases (ADR-006); there is no installer
and no self-update — `swiss update` only compares the running build with the newest release
and prints how to swap the exe (§host.cli). Nothing goes to crates.io: the workspace sets
`publish = false`, since a stray `cargo publish` would claim ten crate names for one binary.
The workflow is `.github/workflows/build.yml`.

## §release.ci — The workflow

| Event | Jobs |
|---|---|
| Pull request or push to master | `check` on Linux and Windows, plus clippy on the arm Mac; `panel`, `deny`, `integration` |
| `v*` tag or a manual run | Everything above, plus `dist` for all five targets; a tag also runs `tag-version` and `release` |
| A change under `docs/` alone | Nothing |

- **Concurrency.** A newer push to the same ref cancels the run in flight; a tag's run always
  finishes.
- **Least privilege.** The workflow is `contents: read`; only `release` gets `contents: write`.
- **`check`** (30 min ceiling, matrix not fail-fast): `cargo clippy --workspace --all-targets
  --locked -- -D warnings`; the duplicate check — print `cargo tree -d -e normal,build`, then
  fail only when `tokio`, `rustls`, `native-tls`, `openssl`, `openssl-sys`, `hyper` or
  `aws-lc-rs` appears twice at column 0 (plain `cargo tree -d` exits 0 whatever it finds, and
  the RustCrypto spread is expected; dev edges never count, §arch.deps); then
  `cargo test --workspace --locked --no-fail-fast`, skipped on macOS (§testing.gaps). Linux
  installs cmake and clang for aws-lc-rs.
- **`panel`** — `npm ci` + `npm run check` on Node 24, independent of every Rust job: the
  committed emit is what ships, so the Rust jobs need no node (§panel.toolchain).
- **`deny`** — `cargo deny check --locked` against `deny.toml` (licence allowlist, advisories,
  registry sources); no Rust build.
- **`integration`** — gate 2 on ubuntu's own dockerd (§testing.it); on failure it prints the
  container list and the MySQL log tail.
- **`tag-version`** — the tag must equal `v` + `[workspace.package] version`.
- **`dist`** — `cargo build --release --locked` for `x86_64-pc-windows-msvc`,
  `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (arm runner),
  `aarch64-apple-darwin`, and `x86_64-apple-darwin` (cross-built on the arm Mac). Both Linux
  targets build on Ubuntu 22.04 runners: a binary needs at least its build host's glibc, so the
  archives hold the floor at glibc 2.35 (Ubuntu 22.04, Debian 12). Each archive
  is `swiss-<version>-<target>` (`.zip` on Windows, `.tar.gz` elsewhere; a manual run's version
  is `dev-<sha8>`) and holds the binary, `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.md` and the
  three skills (`skills/swiss/SKILL.md`, `skills/swiss-remote/SKILL.md`,
  `skills/swiss-memory/SKILL.md`, from `src/skill_assets`).
- **`release`** — needs `tag-version`, `check`, `dist`, `panel`, `deny` and `integration`;
  writes and verifies `SHA256SUMS` over the archives and publishes a GitHub Release with
  generated notes.

TLS: Windows links schannel, unix links rustls, so no unix binary depends on system OpenSSL.
The macOS desktop bundle is assembled by `scripts/package_macos.py` from the matching
Mach-O executable. Its `Info.plist` names `swiss` as the entry point, permits local networking
without disabling transport security globally, and requires macOS 14. The bundle includes
the brand icon and licence notices. Native builds use `codesign --sign -`; cross builds can
use `rcodesign` for the same ad-hoc identity. This is local signing, not Developer ID or
Apple notarization. The transport ZIP preserves the executable's 0755 mode and carries a
SHA-256 sidecar (`scripts/test_package_macos.py`). No user state or credentials enter a bundle.

Dependabot (`.github/dependabot.yml`) proposes cargo, npm (the panel) and Actions updates
monthly; a bump is reviewed like any dependency change (§arch.deps), and a manifest moves
together with its lockfile.

## §release.hygiene — What the repository may contain

- **Authorship.** The only author is `young1lin`: `authors` in the workspace manifest, the
  licence headers and `NOTICE` carry the bare name, and commits carry no `Co-Authored-By`
  trailer. **No file carries an email address**; contact goes through GitHub's private
  channels (§security.reporting).
- **Licence.** Apache-2.0 for the whole tree. Every source file starts with the header
  `Copyright 2026 young1lin`; `scripts/add-apache-headers.ps1` adds it idempotently to files
  that lack it (never to `docs/`, `assets/`, vendored or generated trees). Manifest metadata
  (`authors`, `license`, `repository`, `rust-version`) lives in `[workspace.package]` and the
  members inherit it.
- **Third-party attribution is kept** — a licence obligation, not authorship: `NOTICE`,
  `THIRD_PARTY_NOTICES.md` (the hand-written entries plus a crate → licence table generated from
  the `cargo tree -e normal,build` set and `cargo metadata`, excluding dev-only crates;
  regenerated when `Cargo.lock` changes), the upstream licence text next to each vendored
  panel library (`admin_assets/js/vendor/*/LICENSE`, embedded with them), and the Z.AI notice on
  the prompt texts the zai-vision adapter reproduces (`zai_prompts.rs`).
- **Standard files.** `SECURITY.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md`
  (Keep a Changelog, SemVer), the issue and PR templates, `dependabot.yml`, `deny.toml`, and a
  `rust-version` that is the toolchain the suite is green on.
- **No real environment data.** No real hostnames, users, database names, keys, measurements
  or screenshots of a real instance: fixtures, examples and seeds use neutral names
  (`acme_app_*`, `shop_*`, `jdoe`, a target called `dev`), and screenshots taken against a real
  instance are git-ignored, never committed. State files, logs and recordings are ignored
  (`gateway.config.json`, `managed.json`, `secrets.json`, `env.json`, `mcp-oauth.json`,
  `remote.json`, `master.key`, `/memory/`, `.env*`, `*.log`, `*.cast`).
- **No absolute personal paths.** The repository is `<repo>`, a home directory is `~`, a remote
  example path is `/home/dev/app`; a script finds its files relative to itself.
- **The brand.** The mark is a red (`#DA291C`) rounded square with three tools fanned open —
  no shield, no cross — in `assets/logo.svg`, `assets/logo-wordmark.svg` and
  `crates/swiss-panel/src/admin_assets/logo.svg`, which stay identical. The tagline is "A
  developer's pocket multitool"; the name `swiss` is unchanged.
- **History.** A rewrite of published history is the owner's decision alone. When one happens,
  the short hashes the tree cites are remapped in a single follow-up commit, and the rules above
  are what the rewrite enforces — the replacement tables themselves are never committed.
- **Before going public**, a pass re-checks this list against the whole history, not just the
  working tree.
