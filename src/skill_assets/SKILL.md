---
name: swiss
description: swiss - the loopback gateway on this machine (MCPs, databases, SSH tunnels, jobs, a terminal) and its remote surface - run commands and move files on SSH machines through it. Invoke only when the user explicitly asks.
disable-model-invocation: true
---

# swiss

One small local process on `127.0.0.1:19999` (the port is the user's; `swiss status` says
which). Six things live behind it, one panel in front of them:

- **MCP** - every MCP server the user's AI clients need, served on `/mcp/<name>` with one
  bearer token (`swiss token`). Registered servers are the user's configuration: add or
  change them in the panel, not by hand.
- **Data** - browse the user's databases (tables, rows, queries) from the panel.
- **Tunnels** - SSH connections the gateway keeps open. A *connection id* here is what the
  remote surface below runs through.
- **Jobs** - scheduled runs of any action the gateway knows.
- **Terminal** - a web terminal in the panel (interactive; the remote surface is not).
- **Remote** - non-interactive commands and file transfer on the machines the tunnels reach.
  This is the surface an agent uses, and the rest of this file.

Boundaries, all of them deliberate:

- **Loopback only.** The gateway binds `127.0.0.1` and refuses anything else. It is reached
  from this machine; to reach it from another, forward over SSH - never widen it.
- **Secrets go in, never out.** Passwords, keys and tokens are sealed in the gateway's state;
  no API, log or record shows a value. A remote run's record keeps env *names*, not values.
- **`workspaceRoot` is a guardrail, not a sandbox.** Relative paths resolve under it; absolute
  paths pass through; the command runs as the SSH login user with that user's rights.

The panel is `http://127.0.0.1:19999` (`swiss open`); the CLI is `swiss --help`,
`swiss remote help`. Everything below is the remote surface.

## swiss remote

Targets name a tunnels *connection id* plus an absolute workspace path - never a host, user
or password (those are the tunnel's, sealed).

```bash
swiss remote endpoints                                  # what the transport serves
swiss remote target add build --endpoint conn-1 --root /data/ws/proj --caps exec,sync
swiss remote exec build -- make -j8                     # streams, exits with the REMOTE exit code
swiss remote exec build --timeout 30m -- ./test.sh -k   # everything after -- is ARGV, untouched
swiss remote exec build --cwd /home/dev/app -- ls  # ABSOLUTE paths pass as-is (ssh trust); relative ones resolve under the root
swiss remote sync build                                 # upload a tree (never deletes); .git/ target/ excluded
swiss remote push build app.exe                         # upload one file
swiss remote cat build config.toml                      # print a remote file to stdout
swiss remote write build config.toml < config.toml      # stdin becomes the remote file (overwrite)
swiss remote pull build out/app.bin --to artifacts/app.bin
swiss remote pull build out/dists                       # a directory pulls recursively
swiss run logs 17 -f; swiss run cancel 17               # detached runs: swiss remote exec ... --detach
```

Editing remote files (no PTY - pick by scope):
- one line / regex:  swiss remote exec build -- sed -i 's/old/new/g' conf/app.toml
- whole small file:  swiss remote cat build conf/app.toml > local, edit, then
                     swiss remote write build conf/app.toml < local   (cat caps at 128 KiB; pull for big)
- human editing:     swiss remote pull build conf/ --to conf/ ... push it back after

A repository can carry `.swiss/remote.json` (plain JSON, no secrets) naming targets and
actions like `build`; `swiss remote exec build -- make` then resolves through it.
sudo passes through like any command (no PTY, so it needs NOPASSWD or -n). Full contract: docs/34.

The same five actions are MCP tools (`remote_exec`, `remote_sync`, `remote_pull`,
`remote_cat`, `remote_write`) on the builtin `/mcp/remote` server, mounted while the Remote
plugin is on; a client that has the gateway's token can call them with no shell.

## UTF-8, end to end

- Every remote command runs under `LANG=C.UTF-8` and `LC_ALL=C.UTF-8` unless the caller's
  `--env` (or the tool's `env`) sets them; argv - Chinese paths included - and the output
  stream are UTF-8 bytes in both directions, and a character never splits across a read.
- `swiss remote write` passes stdin through byte for byte: the local file must already be
  UTF-8 (no BOM) if the remote side expects it.
- **Windows PowerShell 5** decodes native output through the OEM code page and shows
  mojibake for anything non-ASCII. Before reading a remote command's output there, run
  `[Console]::OutputEncoding = [Text.Encoding]::UTF8` once in the session (pwsh 7.4+ needs
  nothing). Redirecting there (`>`) re-encodes too; `swiss remote pull` the file instead.

## Every run leaves a record

Each remote action - CLI, MCP tool, panel or job - is recorded when it finishes: who asked
(`cli:<user>@<host>`, `mcp:<token label>`, `panel`, `jobs`), the target, the argv or the
file shape, the env *names*, exit code, duration, and the output stream in a file (the
first 16 MiB, plus the last 64 KiB when it ran past that).
The last **seven days are always traceable**: no size budget removes a record that young
(under pressure the output file goes first, and the record says so). Older records are
kept up to 30 days within the size budget.

```bash
swiss run audit                                   # the last 7 days, one line a run
swiss run audit --since 36h --target build        # a window, one target
swiss run audit --actor mcp:claude-code --json    # one actor, machine-readable
swiss run audit --since 2026-09-14T00:00:00Z --export ./audit-w38   # copy the window out
```

`--export` writes `runs.jsonl` and `out/<id>.txt` for every run in the window - the
material to hand to whoever asked what happened. The panel shows the same record under
Remote › Runs, and the audit line's `#id` is the row there.
