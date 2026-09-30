---
name: swiss-deploy
description: Use when the user asks to ship, deploy, publish, or update the production swiss instance on port 19999 (上线 / 发版 / 部署), or when deciding whether a change is ready to go out through the deploy script.
---

# Deploying to 19999

Deployment is the last step, done once, and it is the operator's decision. Do not deploy because
a change feels finished — the user asks for it. When they do, `scripts/deploy.ps1` owns the whole
order; never improvise a stop/build/start sequence beside it.

## Preconditions (all of them)

1. [swiss-verify](../swiss-verify/SKILL.md) evidence is green for the outgoing diff.
2. User-visible behavior was live-verified on 19998 via
   [swiss-live-verify](../swiss-live-verify/SKILL.md). Deciding "not user-visible" to skip this
   under time pressure is itself the red flag: a diff touching an adapter, the panel API or boot
   is user-visible — when in doubt, it is.
3. [swiss-review](../swiss-review/SKILL.md) found no blockers.
4. The user explicitly asked to deploy — and the ask is current and names this change. Yesterday's
   request, or one about a different change, does not count.

## What the script does (SPEC §host.ops)

1. **The single-deployer lock**, first, held for the whole run.
2. **Gates while production is still up** — `cargo test --workspace`, clippy `-D warnings`,
   gate 2 (real databases, Docker via `DOCKER_HOST`), the panel's `npm run check`. A failing gate
   never takes the daemon down. The gates are a backstop, not a substitute for the evidence above.
3. **`cargo build --release`** into `target\` while the old daemon keeps serving from
   `bin\swiss.exe`.
4. **The outage**: stop, copy the build to `bin\swiss.exe` (retrying while the old process lets go
   of it), `start --no-open`. The last line states how long 19999 was down.
5. **The proof**: the `/health` `build.hash` on 19999 must equal the hash the new exe stamps into
   `--version`, or the script fails loudly with both values. Read it; do not skim past it.

Phase lines also land in the gateway home's `deploy.log`, so a background deploy can be watched.

If the copy or the start fails, production is down: the script's failure line names the command
that starts the daemon again by hand — run it before anything else.

## Options

- `-SkipGates` is a hotfix escape hatch: only on an explicit user decision, and the report says
  so. A gate that cannot run (Docker down for gate 2) is reported the same way.
- If the user prefers to run the deploy from their own terminal, hand them the exact command and
  stay available to read its output.

## After

Report the deployed hash, the health proof and the outage. If something regressed in production,
say so immediately — recovery is a deploy of the last known good commit — then diagnose with
[swiss-debug](../swiss-debug/SKILL.md) away from production.
