# Docs

[`SPEC.md`](SPEC.md) is the one specification of swiss: what the system does **now**, organised
by area, with the decision log (ADR-001 …) in its last section. There are no numbered spec files
and no hand-off prompts; a change to behaviour amends the section it touches, in the same commit
as the code. Code, tests and skills cite it as `SPEC §area.sub`.

| Anchor | Area |
| --- | --- |
| §about | How to read and amend the spec |
| §product | What swiss is, the four properties, the memory record |
| §arch | Crates and edges, runtime model, dependency policy, Rust rules, declared exceptions |
| §formats | The data directory, the sealed envelope, logs, the wire rules |
| §host | Plugin host, lifecycle, routes, the loopback boundary, session, config, vault, groups, CLI, daemon |
| §mcp | The MCP plugin: registry, adapters, endpoint, OAuth, revisions, call log |
| §data | The Data plugin: connections, browsers, SQL console, Redis, tabs |
| §tunnels | SSH connections, forward rules, proxy and jump hosts |
| §jobs | Config-driven scheduled jobs |
| §terminal | Local and remote shells over WebSocket |
| §remote | Remote execution: targets, exec/sync/pull, runs |
| §process | The process plugin and the supervisor |
| §panel | The admin panel: navigation, design system, UI library, TypeScript, i18n |
| §security | The security model in one place |
| §testing | The gates, the suites, the integration harness, live verification |
| §release | CI, release archives, repository hygiene |
| §decisions | ADR-001 … the decision log |

To change the spec, use the `/swiss-spec` skill: interview → amend the section in place →
acceptance tests → delivery order. `assets/` holds the images the repository README shows.
