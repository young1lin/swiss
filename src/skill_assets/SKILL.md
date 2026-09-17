---
name: swiss
description: swiss (the local dev toolbox). Invoke only when the user explicitly asks.
disable-model-invocation: true
---

# swiss

One local process hosting MCP servers on `http://127.0.0.1:19999/<name>`.
State: `~/.mcp-gateway/` (`SWISS_HOME` to override; the legacy `MCP_GATEWAY_HOME` still works).

Do not cat `.env` — it holds database passwords. For the client token, run `swiss token` (or `swiss creds`
for url + token). The panel itself has no login — loopback only.

## Gateway

```bash
swiss status          # up? url, every MCP, memory — or "not running"
swiss start           # detached background service, then opens the panel
swiss start -p 18000  # listen there; saved as the new default
swiss start --no-open
swiss logs            # -f to follow
swiss stop
swiss open            # panel in a browser
swiss token           # bearer token only (one line, for scripts)
swiss creds           # panel url + client token — tell the user these
swiss skill install   # copy this skill to ~/.agents, ~/.claude, ~/.cursor skills dirs
```

On Windows, `swiss status` first — the port refuses a duplicate. `swiss -p <port> <cmd>` for a non-default instance.

## Admin API

Base `http://127.0.0.1:19999`, loopback only (that is the whole gate). MCP endpoints still need
`Authorization: Bearer <token>` from `swiss token`; the `/api` routes need no header.

| Action | Method + path | Body |
| --- | --- | --- |
| List MCPs | `GET /api/mcps` | — |
| Query one | `GET /api/mcps/<name>/details` | — |
| **Add** | `POST /api/mcps` | `{ "name", "type", …fields }` |
| **Import `.mcp.json`** | `POST /api/mcps/import` | the file's JSON (`mcpServers` / `servers` / bare map) |
| **Modify** | `PUT /api/mcps/<name>` | `{ "type", …fields }` |
| Remove | `DELETE /api/mcps/<name>` | — |
| Restart | `POST /api/mcps/<name>/restart` | — |

Import turns stdio `command`+`args` into `proc`, remote `url` into `http`. Names already in use become `redis-1`, `redis-2`. Entries whose URL is this gateway are skipped.

Secrets: put the credential in `~/.mcp-gateway/.env`, reference `${REDIS_PASS}` in the MCP.
Adapter field shapes (redis/mysql/pg/…): `GET /api/mcps` lists live ones with their shapes; the
panel's Add sheet shows every field per type. Add returns `201 {name, type, lifecycle}`;
unset `${ENV}` → `down`, fill it and restart.


## Remote execution (SSH targets)

Run commands on the machines the Tunnels plugin already reaches. Targets name a tunnels
connection id plus an absolute workspace path - never a host, user or password.

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

A repository can carry `.swiss/remote.json` (plain JSON, no secrets) naming targets and
actions like `build`; `swiss remote exec build -- make` then resolves through it.
workspaceRoot is a guardrail, not a sandbox - the command runs as the SSH login user.
sudo passes through like any command (no PTY, so it needs NOPASSWD or -n). Full contract: docs/32.

## Connect a client

**Claude Code**

```bash
claude mcp add --transport http --scope user redis http://127.0.0.1:19999/redis \
  --header "Authorization: Bearer <token>"
```

**Codex** — `~/.codex/config.toml`:

```toml
[mcp_servers.redis]
type = "http"
url = "http://127.0.0.1:19999/redis"
headers = { "Authorization" = "Bearer <token>" }
```

A new MCP is usable after the client reconnects. `readonly: true` unless the user needs writes. Loopback only — SSH-forward the port to reach it from another machine.
