# Changelog

All notable changes to swiss are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-09-25

### Added
- First public release candidate: the gateway (`/mcp/<name>` for every MCP an AI client
  needs — proc, HTTP/OAuth, REST, MySQL, PostgreSQL, Redis, zai-vision), the admin panel in
  English and 简体中文, SSH tunnels with proxy and jump, configuration-driven jobs, the web
  terminal (local PTY and remote SSH), the data browser, the device-bound secret vault,
  agent-friendly remote execution, groups everywhere, and the `swiss` CLI
  (`start` / `stop` / `token` / `creds` / `skill install` / `autostart` / `update` /
  `export` / `import` / `remote` / `run`).
- Redis Streams on the Data page: newest-first windows with entry-id cursor paging, a Follow
  toggle polling `XREVRANGE` into a 500-row ring, and read-only consumer groups (docs/45).
- The panel and `/api/*` need a sign-in (docs/48): `swiss start` and `swiss open` open a
  single-use sign-in link that expires in two minutes and sets a 30-day `HttpOnly`,
  `SameSite=Strict` session cookie; the CLI signs its calls with a key rotated on every start
  and sealed in `session.json`. `swiss api <METHOD> <path> [json]` is the scripted way to call
  the admin API. Another process that only knows the port gets 401.
- Remote addresses are masked in the panel until their eye is pressed: an SSH connection's
  host, a forward's non-loopback target, and a database MCP's host (an endpoint URL's host when
  it is an IP literal) draw as `••••••`, with no tooltip carrying the value, so a screenshot
  shares none. The reveal lasts until the page reloads; loopback draws plain.
- The `swiss-it` integration harness behind feature `it`: real MySQL/PostgreSQL/Redis engines
  through testcontainers, a CI integration job, and gate 2 in `scripts/deploy.ps1` (docs/44).
- `swiss remote exec` resolves vault references: `${secret://name}` in an argv word, an
  `--env` value or `--cwd` is replaced by the stored value on the way out, while the run list,
  the audit and the panel keep the reference as typed, and output that echoes the value comes
  back as `••••••••`. A missing name fails the run before anything is sent (docs/34 R10).
- A vault reference can carry a default: `${secret://name:default}` uses `default` when the
  vault has no such name, everywhere references resolve (docs/19, 2026-09-28).

### Changed
- Remote runs no longer share the two-slot run pool that protects this machine: each remote
  target has its own lane of 8 runs at once (each its own channel on the one SSH connection),
  so parallel terminals driving one server stop getting `429 run capacity is full (2/2
  running)`; local jobs keep `maxConcurrentRuns` (docs/34 R11).
- The Data page's value viewer shows JSON as the panel's highlighted code block, the one Logs,
  Runs and Traffic use, instead of a folding tree; a string that holds JSON is shown decoded,
  and a document longer than 200 lines paints its first 200 with a Show all button.
- A Redis key row and its open tab lead with a glyph for the key's type (string, hash, list,
  set, zset, stream), so `string` and `stream` no longer read alike; the type word stays.

### Fixed
- With Follow on, a Redis stream's new rows replace only the table: the Follow bar was rebuilt
  on every tick, so its interval picker closed itself each second (docs/45 §2.3).
- The Data page's Redis sidebar follows the keyspace: the `r` refresh, a return to the page or
  the connection, and every console command re-walk the key list quietly, as deep as More
  went, with the open key kept. It used to freeze at its first answer, so `SET test 1` on an
  empty Redis never showed up. An empty Redis also lists the database the connection sits on
  (INFO keyspace names only databases holding keys), so the database row is there from the
  first visit (docs/47 D6).
- Replacing a secret now takes effect at once: the vault write rebuilds every MCP that
  references it, which kept the old value resolved in its adapter until the gateway
  restarted. The Secrets page gains Replace value… on each row's ⋯ menu, a Replace label
  when the typed name is already stored, and a toast naming the MCPs that reloaded (docs/19).
- A group made on an empty list now shows up: SSH Connections, Port Forwards, Jobs, Secrets
  and Tokens replaced their whole groups region with a page-level empty state while they had
  no rows, so a new group appeared only in the New sheet's Group select, with no header to
  rename or delete it by. The groups always paint now, as Remote Targets already did (docs/20).
- The Terminal page's target picker names a remote by its connection name alone; it showed
  `user@host`, which put the server's address into every screenshot of a terminal.
- A run being canceled stays visible until it has stopped: cancel took it out of the active
  set before its clean stop finished, so `/api/runs/<id>` answered 404 meanwhile (the CLI
  follower printed `404 no run N` and exited 1) and the next run could start beside a child
  still alive.
- `swiss remote write` no longer records the file's content in the run log
  (`logs/remote/runs.jsonl`); the audit line keeps its size as `contentBytes`.
- A wrong Redis password fails at once as an authentication failure; the connection
  manager's retries hid it behind "redis connect timed out after 5s".
- `/api/tunnels` connection rows now carry `keyPath`: the panel's edit sheet prefills from the
  row, and a custom private-key path is no longer silently rewritten to the default on save.
- Jumping a Redis stream view back to the latest window now voids the follow tick already in
  flight, so a late poll can no longer pool stale rows behind the jump (docs/45 S3).
- The connection-test gate accepts `mariadb` like the panel's Test button always offered it:
  a mariadb Test click answered 400 "no connection test" before.

[0.1.0]: https://github.com/young1lin/swiss/releases/tag/v0.1.0
