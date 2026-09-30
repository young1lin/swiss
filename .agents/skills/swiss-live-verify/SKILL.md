---
name: swiss-live-verify
description: Use when a swiss change needs a real running gateway to be believed — panel behavior, an /api/* response shape, MCP proxying, tunnels, terminal streaming, boot on real state — or whenever live browser or HTTP verification is called for.
---

# Live verification on the isolated 19998 instance

**Port 19999 is production for the human on this machine.** Never stop, restart or redeploy it
while iterating, and never send it iteration traffic — no probe, no POST, no panel click. The
only sanctioned 19999 request is a read-only `GET /health`. The port discipline is AGENTS.md's
"Live testing ports"; this skill owns the procedure.

## The loop

`scripts/live-check.ps1` (Windows) and `scripts/live-check.sh` (macOS/Linux) in this skill
build, boot and prove the served build:

```powershell
& .agents\skills\swiss-live-verify\scripts\live-check.ps1            # build + boot + prove
& .agents\skills\swiss-live-verify\scripts\live-check.ps1 -SkipBuild # reuse the test exe
& .agents\skills\swiss-live-verify\scripts\live-check.ps1 -Fresh     # clean test home first
& .agents\skills\swiss-live-verify\scripts\live-check.ps1 -Stop      # stop by port-owning PID
```

```bash
bash .agents/skills/swiss-live-verify/scripts/live-check.sh              # build + boot + prove
bash .agents/skills/swiss-live-verify/scripts/live-check.sh --skip-build # reuse the test exe
bash .agents/skills/swiss-live-verify/scripts/live-check.sh --fresh      # clean test home first
bash .agents/skills/swiss-live-verify/scripts/live-check.sh --stop       # stop by port-owning PID
```

On Windows the helper wraps the repo's `scripts/test-instance.ps1` (boot and state isolation);
on macOS/Linux `live-check.sh` does the same directly, with the test home at
`$HOME/.swiss-test-home` (`SWISS_TEST_HOME` overrides) and the victim found only through the
port's owning pid (`lsof`/`ss`).

What it does, for when you need the pieces by hand:

1. **Build into `target-test`**: `$env:CARGO_TARGET_DIR = "target-test"; cargo build --release`.
   `target\` is what the next `scripts/deploy.ps1` copies to production.
2. **Pin the MCP bearer** before the start, so `/mcp/*` calls have a known value:
   `$env:SWISS_TOKEN = "acceptance-token-for-1998"` (the literal `scripts/acceptance-16.ps1`
   pins; arbitrary, not a typo).
3. **Start** — `scripts/test-instance.ps1` snapshots the sealed state into
   `%LOCALAPPDATA%\swiss-test-home`; `-Fresh` wipes it first for a first-run scenario. A port
   already held means a stale instance is serving the WRONG build: `-Stop` it, never layer on
   top. Never pass `--port`/`-p`: `start` and `serve` write the port into the config.
4. **Verify.**
   - `/health` needs no auth, and its `build.hash` must equal the exe's `--version` hash — a
     mismatch means a stale instance answered.
   - `/api/*` needs the admin session (SPEC §host.session), never the bearer. With
     `SWISS_HOME` = the test home and `SWISS_PORT=19998`: `swiss api GET /api/info` for
     scripted calls, and `swiss api POST /api/session/ticket` for the single-use `url` that
     signs a browser in.
   - Every `/api/*` response keeps the shape the panel reads
     (`crates/swiss-panel/panel/src/`).
   - `scripts/acceptance-16.ps1` is the full boot / health / env-scrub sweep against 19998.
5. **Stop by the port's owning PID**: `scripts/test-instance.ps1 -Stop`. `Get-Process swiss`
   kills the user's 19999 daemon too.

## Discipline

- A save on 19998 writes the test home. If a scenario claims isolation, prove it —
  acceptance-16 checks the production config's mtime is unchanged.
- State that must survive a restart is verified by stopping and starting the instance, not by
  assuming.
- Carry the evidence forward (commands, responses, build hash), not raw HTTP scrollback or
  browser dumps.
- Report which build hash served, what was checked, the exact responses, and that 19999 was
  untouched.
