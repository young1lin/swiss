# Contributing

Thanks for looking. swiss is small on purpose — one binary, one loopback port, a memory budget
that is a feature — so contributions are judged by whether they keep it that way as much as by
whether they work. The house rules live in [AGENTS.md](AGENTS.md); this page is the short human
version.

## Build

- Rust stable, at least the `rust-version` in `Cargo.toml`. `cargo build --release` yields the
  one `swiss` binary; nothing else is needed to build or run it.
- Node 24 is a **dev** dependency of the admin panel only: `crates/swiss-panel/panel` holds the
  TypeScript sources and the vitest suite, and `npm run build` there emits the plain ES modules
  that are committed under `crates/swiss-panel/src/admin_assets/js/` and embedded by cargo. The
  emit is committed so that `cargo build` never needs node.

## The gates

Every change passes all three before it is proposed:

```
cargo test --workspace
cargo test -p swiss-it --features it   # real-database changes; needs Docker (docs/44)
cargo clippy --workspace --all-targets -- -D warnings
cd crates/swiss-panel/panel && npm ci && npm run check   # panel changes
```

`--workspace` is load-bearing: without it cargo builds the root package alone, runs a small
minority of the suite and still reports ok.

The second `cargo test` is gate 2 (docs/44): it starts real MySQL, PostgreSQL and Redis
through Docker (`DOCKER_HOST`) or the `SWISS_IT_*_URL` overrides, and a diff touching the
database adapters, browsers or `crates/swiss-it` itself is not done without it. A machine
without Docker runs the other gates and says so in the PR.

## Rules that are not negotiable

- **Loopback only.** Nothing widens the bind, relaxes the `Host`/`Origin` checks or adds a way
  to reach the gateway from another machine. Forward the port over SSH instead.
- **Secrets never come back out.** A value stored in the vault is write-only; masked fields
  stay masked in every API answer, log line and error.
- **Sealed formats are frozen.** `docs/05` names the on-disk and on-wire shapes that must stay
  byte-compatible; `tests/fixtures` proves it.
- **Panel edits go to `panel/src/*.ts`, never to the emitted `js/`.** Rebuild, and walk the
  change in a real browser (see `.agents/rules/panel-proof-of-life.md`). Both languages: the
  panel is English and 简体中文, and every visible string has a key in both dictionaries.
- **No new runtime the binary has to carry.** A dependency that pulls a second async runtime,
  TLS stack or scripting engine is refused; `.agents/skills/swiss-dependency-review` explains
  what a manifest change has to show.

## Proposing a change

1. Open an issue first for anything larger than a fix; the `docs/NN-*-spec.md` files show the
   level at which design decisions are written down here.
2. Branch from `master`. Keep commits self-contained; the message says what changed and why,
   in the voice of the existing log.
3. Open a pull request against `master` with the gates green. A panel change includes what the
   real-browser walk showed, in both languages.

By contributing you agree that your contribution is licensed under the Apache License 2.0,
the license of the project (see [LICENSE](LICENSE) and [NOTICE](NOTICE)).
