---
name: swiss-deploy
description: Use when the user asks to ship, deploy, publish, or update the production swiss instance on port 19999 (上线 / 发版 / 部署), or when deciding whether a change is ready to go out through the deploy script.
---

# Deploying to 19999

Deployment is the last step, done once, and it is the operator's decision. Do not run the deploy
because a change feels finished — the user asks for it. When they do, `scripts/deploy.ps1` owns the
entire order; do not improvise a stop/build/start sequence beside it.

## Preconditions (all of them)

1. [swiss-verify](../swiss-verify/SKILL.md) evidence is green for the outgoing diff.
2. User-visible behavior was live-verified on 19998 via
   [swiss-live-verify](../swiss-live-verify/SKILL.md). Deciding "not user-visible" to skip this
   under time pressure is itself the red flag: a diff touching an adapter, the panel API, or boot
   is user-visible — when in doubt, it is.
3. [swiss-review](../swiss-review/SKILL.md) found no blockers.
4. The user explicitly asked to deploy — and the ask is current and names this change (or the diff
   it contains). Yesterday's request, or a request about a different change, is not this
   precondition.

`deploy.ps1` runs the test/clippy gates itself while production is still up — a failing gate must
not take the daemon down — but its gates are a backstop, not a substitute for the evidence above.

## What the script does (and why the order is fixed)

- Gates (`cargo test --workspace`, clippy `-D warnings`).
- **Stop before build**: the linker cannot overwrite the exe a running daemon holds
  (`os error 5`). Exit 3 from `swiss stop` means nothing was running, which is fine to deploy
  over; exit 1 means the stop was refused — resolve it by hand, then re-run.
- `cargo build --release` into the main `target/` tree (not `target-test/`).
- `swiss start --no-open`, then `swiss status`.
- **The proof**: the `/health` `build.hash` on 19999 must equal the hash this build stamped into
  `swiss --version`, or the script fails loudly with both values. Read it; do not skim past it.

If the build fails after the stop, production is down — restart the old daemon by hand
(`& target\release\swiss.exe start --no-open`) before doing anything else.

## Options

- `-SkipGates` is a hotfix escape hatch. Only with an explicit user decision, and say so in the
  report.
- If the user prefers to run the deploy from their own terminal, hand them the exact command and
  stay available to read its output.

## After

Report the deployed hash, the health proof, and treat the event as a deployment, not a test. If
something regressed in production, say so immediately — recovery is `swiss stop` + rebuild at the
last known good commit + `swiss start`, then diagnose with [swiss-debug](../swiss-debug/SKILL.md)
away from production.
