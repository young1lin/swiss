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
- The `swiss-it` integration harness behind feature `it`: real MySQL/PostgreSQL/Redis engines
  through testcontainers, a CI integration job, and gate 2 in `scripts/deploy.ps1` (docs/44).

### Fixed
- `/api/tunnels` connection rows now carry `keyPath`: the panel's edit sheet prefills from the
  row, and a custom private-key path is no longer silently rewritten to the default on save.
- Jumping a Redis stream view back to the latest window now voids the follow tick already in
  flight, so a late poll can no longer pool stale rows behind the jump (docs/45 S3).
- The connection-test gate accepts `mariadb` like the panel's Test button always offered it:
  a mariadb Test click answered 400 "no connection test" before.

[0.1.0]: https://github.com/young1lin/swiss/releases/tag/v0.1.0
