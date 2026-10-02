# Changelog

All notable changes to swiss are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- MongoDB connections (`"type": "mongo"`, a `mongodb://` URL): six tools — `mongo_list_collections`,
  `mongo_find`, `mongo_aggregate`, `mongo_describe_collection`, `mongo_command` (server-damaging,
  session and cursor commands refused; dropping data needs `allowDestructive`) and `mongo_inspect`
  (activity, slow queries, per-collection time, unused indexes, storage, server, replication) —
  plus `mongo://` resources.
- The Data page's documents workspace for MongoDB: collections in Collections / Views / System
  bands, a query bar in the shell's syntax (filter, projection, sort, skip, field completion),
  documents as a list, canonical JSON or a table, an editor that writes back only if nobody
  changed the document since it was read, multi-document writes in one transaction on a replica
  set, bulk update and delete, an aggregation pipeline builder with per-stage previews, schema
  analysis, indexes with usage, explain plans with a collection-scan verdict, validation rules,
  JSON / NDJSON / CSV export and import, and a command console with templates.

## [0.1.2] - 2026-10-02

### Added

- `swiss path [on|off]` and a "Put swiss on your PATH" switch in the panel: this exe's folder in
  the current user's PATH on Windows, a `~/.local/bin/swiss` symlink elsewhere. A PATH entry
  added by hand counts as on.
- `pg_inspect` and `mysql_inspect`: one named diagnostic check per call — running statements,
  lock waits with their blockers, top queries, sequential scans, unused indexes and tables
  without a primary key, cache hit ratio, connections, plus vacuum (PostgreSQL) or
  fragmentation (MySQL). A missing privilege is answered with the grant that fixes it.
- `mysql_describe_table`: the table's own `CREATE` statement plus the foreign keys that point
  at it.

### Changed

- `pg_describe_table` answers in one query with the primary key, indexes, foreign keys both
  ways and constraints; `schema` is optional and resolves through the `search_path`.
- `mysql_list_tables` takes a `database` and names it once at the top of the reply.
- `redis_scan` takes `limit` (keys to collect, default 100) instead of `count`, and keeps
  scanning until it has them — a sparse pattern no longer returns empty pages.

### Fixed

- Running the test suite no longer turns start at sign-in off on the machine that runs it; the
  tests write no OS registration of the operator's.
- `swiss autostart` no longer runs its label into the registry location it prints.

## [0.1.1] - 2026-10-02

### Fixed

- The Linux archives are built on Ubuntu 22.04 and run on glibc 2.35 or newer (Ubuntu 22.04,
  Debian 12, and later); 0.1.0's needed glibc 2.38.

## [0.1.0] - 2026-10-02

The first release.

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

[Unreleased]: https://github.com/young1lin/swiss/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/young1lin/swiss/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/young1lin/swiss/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/young1lin/swiss/releases/tag/v0.1.0
