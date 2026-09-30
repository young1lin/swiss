---
name: swiss-dependency-review
description: Use when adding, upgrading, or removing a cargo dependency in this workspace, editing any Cargo.toml, or evaluating whether a crate should be pulled in — before the manifest change is committed.
---

# Dependency weight review

Memory is the product. Every new dependency justifies its weight, and the justification belongs in
the commit message (or the PR description) — measured, where a number exists, not asserted.

## The rules

1. **`default-features = false` first**, then add back exactly what is needed — wherever the
   defaults pull real weight. The workspace already does this for the heavyweights (`tokio`,
   `axum`, `rmcp`, `chrono`, `time`, …); match them. Note the standing exception: `rmcp`'s
   `macros` feature stays off on purpose; servers implement `ServerHandler` by hand, mirroring
   the Node build's `setRequestHandler`.
2. **Forbidden classes — a crate that pulls any of these is a bug, not a dependency:**
   - a second TLS stack,
   - a second async runtime,
   - its own thread pool (the runtime is `current_thread` by design; reaching for
     `multi_thread` itself needs a measured reason in the commit message).
3. **`cargo tree -d` is read, not assumed.** A duplicated **TLS stack or runtime** fails this
   review outright; other duplicates are judged, not zero-tolerated — the tree currently carries a
   known benign `base64` pair (0.22 via axum, 0.23 direct), so compare the output before and
   after your change rather than demanding absolute cleanliness.
4. **Inspect before adding:** `cargo tree -i <crate>` and the crate's feature list — check what the
   enabled features actually pull in transitive weight, and prefer the smaller feature set even
   when the default would be more convenient.
5. **No subprocess where a syscall exists** — the repo's own rule quotes ~65 MB of transient
   working set per spawn; a crate that shells out where a direct API call exists pays it too.
   Reject it or gate it behind the same excuse standard as a hand-written
   `Command::new("powershell")`.

Rationalizations that don't hold here: "it's only a dev-dependency" (still costs CI time and
lockfile review); "it's tiny" (tiny is about the crate, not its transitive closure — which is
exactly what rule 4 inspects).

## Procedure

1. State what capability the dependency buys and what it replaces (hand-rolled code, an existing
   heavier dependency it can displace, or nothing).
2. Add it with the minimal feature set.
3. Run the gates: `cargo build --release`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo tree -d` — manifest changes
   are cross-cutting, so the full run is the honest one
   ([swiss-verify](../swiss-verify/SKILL.md)).
4. Record the weight decision where the next reader will find it: the manifest comment beside the
   dependency (like the existing `rmcp`/`swiss-terminal` notes) and/or the commit message. A
   decision that moves a forbidden-class boundary (TLS, crypto, runtime, thread pool) also gets an
   ADR in SPEC §decisions — ADR-013 is the precedent.
