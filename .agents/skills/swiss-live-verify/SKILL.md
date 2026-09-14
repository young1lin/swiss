---
name: swiss-live-verify
description: Use when a swiss change needs a real running gateway to be believed — panel behavior, an /api/* response shape, MCP proxying, tunnels, terminal streaming, boot on real state — or whenever live browser or HTTP verification is called for.
---

# Live verification on the isolated 19998 instance

**Port 19999 is production for the human on this machine.** Never stop, restart, or redeploy it
as a step of iterating on a change, and never send iteration traffic to it either — no probe, no
POST, no panel click against production while developing; the only sanctioned 19999 request is a
read-only `GET /health`, exactly what `acceptance-16.ps1` allows itself. The full port
discipline lives in AGENTS.md's "Live testing ports" section; this skill owns the procedure.

## The loop

`scripts/live-check.ps1` (Windows) and `scripts/live-check.sh` (macOS/Linux) in this skill run
steps 1-4 and prove the served build — pick your platform's variant:

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

On Windows the helper wraps the repo's `scripts/test-instance.ps1` (which owns boot + state
isolation); on macOS/Linux `live-check.sh` implements the same procedure directly — the repo's
own instance script is Windows-only. The POSIX test home is `$HOME/.swiss-test-home` (override
with `SWISS_TEST_HOME`); stopping finds its victim only through the port's owning pid
(`lsof`/`ss`), never by process name.

Manually, the pieces:

1. **Build the test exe into its own target tree** (the 19999 daemon holds `target/release/swiss.exe`
   — iteration must not fight it for the file): `$env:CARGO_TARGET_DIR = "target-test"; cargo
   build --release`.
2. **Pin the token before starting.** The snapshotted production config carries the legacy
   `tokenEnv: MCP_GATEWAY_TOKEN`, and the named variable resolves first — so pin that name and
   the whole live session has a known bearer: `$env:MCP_GATEWAY_TOKEN = "acceptance-token-for-1998"`
   (the literal acceptance-16.ps1 pins; arbitrary, not a typo).
3. **Start the instance** — snapshot of real state by default, clean home when the change needs a
   first-run or bootstrap scenario. If the port is already held, a stale instance from an earlier
   session is serving the WRONG build — `-Stop` it first, never layer on top.

   ```powershell
   scripts/test-instance.ps1            # snapshot sealed state, serve on 19998
   scripts/test-instance.ps1 -Fresh     # wipe the test home first
   ```

   Never pass `--port`/`-p` to `swiss` yourself: both `start` and `serve` write the port
   into the config.
4. **Verify.** `/health` needs no auth; every `/api/*` route wants the pinned bearer. The
   `/health` `build.hash` must match the exe this run built — a mismatch means a stale
   instance answered. `swiss creds` (with `SWISS_HOME`/`SWISS_PORT` pointed at the test home)
   prints the panel URL and token for browser work.

   - The panel at `http://127.0.0.1:19998` is the spec for the admin API: responses must be
     shape-identical to what the Node build returns (see
     [swiss-node-reference](../swiss-node-reference/SKILL.md)).
   - For the full boot/health/env-scrub sweep, `scripts/acceptance-16.ps1` drives H1/H2/H3
     against 19998 and never touches 19999 beyond a read-only health check.
5. **Stop by the port's owning PID, never by process name** — `Get-Process swiss` kills the
   user's 19999 daemon too: `scripts/test-instance.ps1 -Stop`.

## Discipline

- A save performed on 19998 writes the test home; if a scenario claims isolation, prove it —
  acceptance-16 checks the production config's mtime is unchanged (steps [1] and [6]).
- State-file behavior (`gateway.config.json`, `managed.json`, tunnels, jobs) that must survive a
  restart is verified by stopping and re-starting the instance, not by assuming.
- Transcript hygiene: raw HTTP scrollback and browser dumps are noise to every later phase —
  carry the evidence (commands, responses, build hash) forward, not the scrollback.
- Report: which exe (build hash) served, what was checked, the exact responses, and that 19999 was
  untouched.
