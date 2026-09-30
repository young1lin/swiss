---
name: swiss-add-plugin
description: Use when adding a new capability, tool, page, or route to swiss — a new panel tab, HTTP endpoints, an MCP tool or adapter, a jobs producer, a CLI surface — or any feature that must reach the panel without editing the host.
---

# Adding a capability the toolbox way

A new tool reaches the panel by contributing a **descriptor, actions, pages and resources** through
the host contract (SPEC §host.plugins) — never by editing a match arm in the host or the composition root
beyond registration. If it seems to need an edge between two subsystem crates, the host contract is
missing something: add it to `swiss-host` instead.

## Before writing code

Name the plan in three lines: which crate owns it, what it contributes (descriptor / action /
page), what it costs at idle, and which host-contract seam it uses. If it won't fit one sitting
and one commit, amend its section of `docs/SPEC.md` first through **swiss-spec** (the repo's
spec-first convention). An explicit nod from the operator is required before anything that adds a
dependency, a child process, or a new crate.

## The shape to follow

- **Edges are the architecture:** `swiss-core ← swiss-host ← {mcp, data, tunnels, jobs, terminal,
  panel} ← swiss`. `swiss` (root `src/`) is composition and nothing else. No peer-to-peer
  dependencies between the six subsystem crates.
- **Three ways to bring a tool in:** a stdio child process, an HTTP endpoint, or Rust compiled
  straight into the binary. The first two are how foreign tools arrive; native is how tools worth
  keeping end up shipping (it costs almost nothing).
- **Three states, switchable at runtime:** enabled, running, visible. Disabling a plugin must
  really release what it held — tasks, connections, children, routes — not just hide a tab.
- **Lazy by default.** Nothing starts eagerly "for simplicity"; the `proc` MCP idling at boot and
  waking on first request is the product's biggest memory feature. A new connection, cache, or child
  is created on demand and released when idle.
- **One action, many entries.** Generic actions are callable from pages, CLI, and jobs; do not copy
  business logic per entry point. Jobs-specific contracts (definitions, triggers, recovery) are in
  SPEC §jobs.
- **A plugin owns at most 5 pages** (SPEC §panel.nav): its pages sit in the context bar as underline
  tabs, and more than ~5 means L3 content is being spent on L2 — restructure the pages instead of
  overflowing into the bar's `⋯` seat.
- **Give the plugin a sprite glyph:** one line in the `GLYPHS` table in
  `crates/swiss-panel/panel/src/plugin-palette.ts` maps the group id to an existing `i-*` symbol
  (the rail is icon-only, SPEC §panel.nav — without a glyph the seat falls back to the puzzle piece,
  which cannot tell two such plugins apart).

## Non-negotiables for anything new

The load-bearing rules in AGENTS.md own the details; the ones a new capability most easily breaks:

- Loopback-only is security: bind `127.0.0.1`, refuse non-loopback `Host`/origin on every new
  route, refuse a non-loopback `host` at config load. `/api/*` routes carry bearer auth.
- Credentials enter config as `${ENV_VAR}` or `secret://name` references, never literals.
- No `serde_json::Value` on a forwarding path — route on envelope fields, pass payloads as
  `&RawValue`. The runtime is `current_thread`; no `unsafe`; no `.unwrap()` on anything
  touching config, network, database or filesystem.
- **The panel is edited here, directly.** Panel JS/HTML/CSS lives in
  `crates/swiss-panel/panel/src/*.ts` with the emit committed under `src/admin_assets/js` (ADR-024) — run `npm run build` there after editing, still no bundler;
  [swiss-ui-design](../swiss-ui-design/SKILL.md) owns its language and checklists. A panel
  change ships with its vitest case in `crates/swiss-panel/panel/test/`, and every `/api/*`
  response stays shape-identical to what the panel reads — a shape change ships on both sides in
  one commit.

## Evidence

- Every behavior change ships with a test that fails before and passes after
  ([swiss-verify](../swiss-verify/SKILL.md)); integration tests drive the axum app through
  `oneshot` — no real port, no real sleep.
- Lifecycle claims (disable really releases) get a test that observes the release, not one that
  trusts the plugin's self-report.
- User-visible surfaces get a live pass on 19998
  ([swiss-live-verify](../swiss-live-verify/SKILL.md)); anything that could move idle memory gets
  a [swiss-memory-record](../swiss-memory-record/SKILL.md) measurement.
- New dependencies go through [swiss-dependency-review](../swiss-dependency-review/SKILL.md).
