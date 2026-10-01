# Changelog

All notable changes to swiss are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The first release, 0.1.0.

### Added
- The gateway: every MCP an AI client needs on `/mcp/<name>` behind a bearer token — stdio
  children (idle until their first request, reaped after idling), remote HTTP with OAuth, REST
  APIs declared in config, MySQL, PostgreSQL, Redis and zai-vision. **Import .mcp.json** brings
  an existing client config across, and each MCP copies a ready client command.
- The admin panel, embedded in the binary, in English and 简体中文. It needs a sign-in: a
  single-use link from `swiss start` / `swiss open` sets a session cookie that only the panel's
  own pages can use, and the CLI signs its calls with a key rotated on every start (`swiss api`
  for scripts).
- Data: tables and rows with buffered edits, a SQL console with server-side completion, export
  and import, structural operations, live sessions; Redis keys, streams with Follow and
  read-only consumer groups, and a console that completes commands and keys.
- Tunnels: SSH connections, forward rules, proxy dialling and jump hosts, and MCPs that ride a
  tunnel.
- Jobs: configuration-driven scheduled jobs with overlap, misfire and capture modes and a run
  history.
- Terminal: local PTY and remote SSH shells in the browser, with recording.
- Remote execution: targets with declared capabilities, `swiss remote exec / sync / push / cat
  / write / pull` and `swiss run`, a run log with an audit, `/mcp/remote`, and the Targets and
  Runs pages; each target runs in its own lane.
- The secret vault: `${secret://name}` (with an optional `:default`) and `${ENV_VAR}` references
  resolved only at use time; stored values are write-only and masked wherever they could come
  back out, `swiss export` included. Remote addresses stay masked in the panel until revealed.
- Groups on every list, start at sign-in (`swiss autostart`), `swiss update` to check for a new
  release, `swiss export` / `import` to move machines, and the embedded `swiss` and
  `swiss-remote` AI skills (`swiss skill install`).
