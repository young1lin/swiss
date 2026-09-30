---
name: swiss-dependency-review
description: Use when adding, upgrading, or removing a cargo dependency in this workspace, editing any Cargo.toml, or evaluating whether a crate should be pulled in — before the manifest change is committed.
---

# Dependency weight review

Memory is the product. Every new dependency justifies its weight, and the justification belongs in
the commit message — measured where a number exists, not asserted. The policy is SPEC §arch.deps
(the main crates and why, what was deliberately not taken); this skill is the review.

## The rules

1. **`default-features = false` first**, then add back exactly what is needed. The workspace
   already does this for the heavyweights (`tokio`, `axum`, `rmcp`, `chrono`, `time`, …); match
   them. `rmcp`'s `macros` feature stays off: servers implement `ServerHandler` by hand.
2. **Forbidden classes — a crate that pulls any of these is a bug, not a dependency:** a second
   TLS stack, a second async runtime, its own thread pool (the runtime is `current_thread`;
   `multi_thread` itself needs a measured reason in the commit message).
3. **`cargo tree -d -e normal,build` is read, not assumed.** A duplicated TLS stack or runtime
   fails the review outright. Other duplicates are judged, not zero-tolerated: the RustCrypto
   generation split (ADR-013) and a few small pairs (`base64`, `getrandom`, `hashbrown`, `rand`,
   `syn`) are already there, so compare the output before and after your change.
4. **Inspect before adding:** `cargo tree -i <crate>` and the crate's feature list — what do the
   enabled features pull in transitively? Prefer the smaller feature set even when the default
   is more convenient.
5. **No subprocess where a syscall exists.** A PowerShell spawn costs ~65 MB and ~350 ms
   (SPEC §tunnels.api, `GET /port`); a crate that shells out where a direct API exists pays the
   same. Reject it, or hold it to the excuse standard of a hand-written
   `Command::new("powershell")`.
6. **Mature libraries over hand-rolled grammars** (SPEC §arch.deps): anything with a real grammar
   — shell quoting, JSON, SQL literals, protocol framing — uses a maintained crate.

Rationalizations that don't hold: "it's only a dev-dependency" (it still costs CI time and
lockfile review); "it's tiny" (tiny is about the crate, not its transitive closure — which is
what rule 4 inspects).

## Procedure

1. State what the dependency buys and what it replaces (hand-rolled code, a heavier dependency it
   can displace, or nothing).
2. Add it with the minimal feature set; commit the regenerated `Cargo.lock` with the manifest.
3. Run the gates: `cargo build --release`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo tree -d -e normal,build` —
   a manifest change is cross-cutting, so the full run is the honest one
   ([swiss-verify](../swiss-verify/SKILL.md)).
4. Record the weight decision where the next reader finds it: a manifest comment beside the
   dependency and the commit message. A decision that moves a forbidden-class boundary (TLS,
   crypto, runtime, thread pool) also gets an ADR in SPEC §decisions — ADR-013 is the precedent.
