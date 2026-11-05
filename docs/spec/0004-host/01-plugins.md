# Plugins, routes and the wire

Part 01 of [§host](README.md), chapter 0004 of the [swiss specification](../../SPEC.md).

## §host.plugins — The plugin contract

Every plugin contributes six things and owns one scope:

1. **Descriptor** — id, kind, label, version, config schema version, default-enabled,
   `requires` (capability names), `restart_on_config_change`.
2. **Config** — a typed schema with defaults, validation and migration; the schema is data the
   panel can render. The backend is the authority; the panel is not a validator.
3. **Services / actions** — capabilities registered by stable name, scoped to the plugin.
4. **Routes** — its own API prefixes, always inside the host's boundary (§host.routes).
5. **Pages** — zero or more page contributions (§panel.nav): id, label, order, path, entry,
   sidebar, layout. A page is not a backend module boundary.
6. **Lifecycle** — create / start / apply_config / stop, plus health, last error and resource
   counts.

`PluginScope` owns the resources: supervised task handles, the cancel signal, registration
tokens, connection leases, child-process ownership, caches. Many tasks per plugin are fine;
losing a task's owner is not. The descriptor is resident; the instance is not — a disabled
plugin keeps nothing expensive. Public contracts are typed traits and DTOs; `serde_json::Value`
appears only at the config and admin boundary.

**Registration.** `src/builtin.rs` is the one composition table (the built-ins in
`register_all`, process the last line, §process; the terminal, remote, llm and memory
factories, from `crates/swiss-plugins/`, register in `server.rs`, and process's factory lives in
`crates/swiss-plugins/src/process.rs`, its register line inside `register_all`). Adding a
plugin is one factory plus one register line — no
match arm in the host, no edit under `crates/swiss-host/src/host/`. Duplicate ids, route
prefixes, action names and page ids are refused loudly at registration; a silent shadow route
is a boundary hole.

**Plugins that need plugins** are expressed as data, never as crate edges:

- `requires` names a capability; the host's capability probe answers met/unmet and the
  inventory carries `requires` + `requiresMet`. A route of a plugin whose requirement is unmet
  gets a refusal naming the dependency. There is no blocking `WaitingDependency` state.
- **Capability seats** (§host.seats) — a provider registers, a consumer leases. Seats live in
  `RuntimeServices` and outlive both plugin instances.
- Read-only bridges between subsystems (tunnels seeing the registry's MCP names) live in the
  composition crate behind read-only traits (`src/mcp_link.rs`).

**The built-ins and their relations:** `mcp` (registry, catalog provider), `tunnels` (shell
and remote-transport provider), `data` (pure consumer, `requires: ["connection-catalog"]`),
`jobs` (a producer on the shared run pool, owner `jobs`), `process` (the capability provider of
`process.exec`, plus the Process page: managed apps and the children view — §process),
`terminal` and `remote` (the public-contract plugins; remote
deliberately declares no `requires`, so its target table stays editable while tunnels is off),
`llm` (§llm: providers, the embedding model, agents and their telemetry, one shared state its
consumers read) and `memory` (§memory: mounts its own built-in MCP entry the way remote does;
`requires: ["llm"]` for the semantic tier and sync runs — a badge, never a refusal).

**The acceptance test for the contract:** a new tool is added without touching host business
logic, the jobs scheduler or the panel shell. Disabling it makes jobs that reference its
action show dependency-unavailable while everything else keeps working. Loading third-party
code at runtime (DLL, WASM, a Node plugin host) is out of scope (ADR-001); a new backend
implementation needs a rebuild.

## §host.lifecycle — States, start, stop, config changes

States: `Disabled` / `Idle` / `Starting` / `Active` / `Stopping` / `Failed`
(`host/descriptor.rs`). Enabled, running and visible are three different things.

- **Single-flight.** One tokio mutex per plugin serializes lifecycle operations; a concurrent
  second start queues, re-reads the world and joins the first result.
- **Start** = `validate_config_for_start` → create → `scope.start`, under `START_TIMEOUT` 30 s.
  Timeout or failure shuts the scope down and records `Failed` + `lastError`. **One failing
  plugin is a status row, never a failed boot**; only the host's own security or config being
  unusable fails the boot.
- **Stop really releases.** Leave the route slot first (in-flight requests now see
  not-serving) → stop new triggers → `instance.stop()` under `STOP_TIMEOUT` 10 s → drain or
  cancel in-flight work → reap children and join readers → flush bounded logs → unregister
  contributions and release leases → `scope.shutdown()` (cancel, 5 s grace per task, abort) →
  drop the instance. Consumers stop before providers. Routes never capture an expensive
  instance behind an `enabled` bool: paths stay mounted and dispatch asks live state.
- **The honesty rule.** A plugin wrapper may stop short of a full unload (the MCP registry and
  the tunnel store stay constructed; they hold no sockets), but its comment says item by item
  what disable really does, and the inventory never claims more. Re-enabling replays the start
  decisions: a panel Stop and a lazy MCP stay as they were.
- **Config change** — the running instance has first claim: `apply_config` applies in place
  (jobs: an edit neither bounces the instance nor cancels runs); `Failed` keeps the instance
  running, the revision unmoved and answers `applied: false`; `NotApplicable` restarts when the
  descriptor says `restart_on_config_change` (tunnels, terminal) and otherwise only records the
  revision (mcp, data, jobs).

**Boot order** (`src/server.rs::run_gateway`; launcher noise is already scrubbed,
§host.daemon): log who started the gateway → PATH repair → first run → inject the env store
and the vault (two lookup paths; vault values never enter the env overlay) → load config →
port probe **before** orphan reaping → call log, registry, managed store → config and managed
MCPs registered, not started (the start decision is the mcp plugin's) → runtime services →
job system → tokens → traffic restore → the admin session (fatal if it cannot be sealed) →
group scopes → plugins registered (process last among the built-ins, so it starts first:
providers before consumers) → **bind** → `start_enabled()` spawned behind the open port in
reverse registration order → the hourly log sweep. Plugins start after the
bind so one slow plugin is never the whole gateway's outage; a request to a plugin still
starting gets the boundary's structured 503.

**Teardown:** `POST /api/shutdown` (Windows has no SIGTERM; a watch channel is the signal) or
ctrl-c → 3 s for in-flight requests → plugins stop in reverse registration order (tunnels
before the registry, so forwarded ports release while MCP connections drain) → runtime
services (unowned runs too) → registry close → flush the call log and traffic.

## §host.routes — Route trees and host-owned routes

Two trees — the main tree (panel, health, `/api/*` admin, `/api/db`, `/mcp/*`) and the extra
tree (tunnels, jobs, actions and runs, terminal, remote, knowledge, llm, process) — each carry the
same layers, outermost first: the **loopback guard** (§host.boundary) → the **admin session gate**
(§host.session) → the **plugin boundary** (a prefix owned by a plugin that is not serving → a
structured 503 naming it; the longest owned prefix wins). Security answers before plugin state
does. Both trees also carry a 2 MiB body limit (413 `request body exceeds N bytes`). An axum layer
covers only the routes that exist when it is attached, so each tree gets its layers separately
(`src/app.rs::build_app`). Unknown paths and wrong methods both answer 404 `{error: "no route
for …"}`, never axum's bare 405; the old root spelling of a known MCP gets a moved-hint 404.

**Registration refuses conflicts, loudly.** A duplicate plugin id, a route prefix that overlaps
a host-owned route, and one that overlaps another plugin's prefix are all errors at
registration (`host/engine.rs`): a silently shadowed route would be a boundary hole.

**Host-owned routes** (a plugin may not claim them; `/api/plugins`, `/api/actions` and
`/api/runs` stay answerable while their providers are stopped):

| Route | Contract |
|---|---|
| `GET /api/plugins` | `{revision, plugins[], pages[]}`; each row `{id, kind, label, version, enabled, state, configRevision, pages[], lastError?, requires?, requiresMet?}`. Compiled (the descriptor), desired (the store row) and actual (`configRevision`) drifting apart is the visible gap of a restart or apply that has not happened |
| `POST /api/plugins/{id}/enable\|disable` | write the row, then reconcile; disable really stops the instance |
| `GET/PUT /api/plugins/{id}/config` | GET `{config, schema, revision}`; PUT validates (400, nothing persisted), CAS-writes (409), reconciles, answers `{revision, config, schema, warnings, applied[, error]}` |
| `/api/actions`, `/api/runs…` | §host.actions |
| `/api/tokens…` | §host.tokens — host-owned though the Token page sits in the MCP group, so disabling MCP takes the page off the air but leaves the credential API and CLI serving |
| `/api/secrets…` | §host.vault |
| `/api/groups/{scope}…` | §host.groups |
| `GET /api/info` | `tokenEnv`, `panelVersion`, `build` — the panel reloads itself when the build changes |
| `GET /api/bus` | the event-bus upgrade (SPEC §bus): one WebSocket per page, one protobuf frame per message. Behind the session gate, and the upgrade must carry an `Origin` naming this `Host` (SPEC §bus.security) |
| `GET/PUT /api/autostart`, `GET/PUT /api/user-path` | the two OS-level settings (§host.cli); 503 in a composition that mounted no provider |
| `GET /api/memory[?tree=1]` | §host.memory |
| `POST /api/shutdown` | answers first, then signals |
| `POST /api/session/ticket` | §host.session |
| `GET /health`, `/health/check` | unauthenticated; carries `build` (a short hash on loopback is acceptable) |

`GET /` serves the embedded panel shell with `no-store`, `GET /admin/*` its assets with `no-cache`
and an ETag (§panel). The MCP client
endpoints `/mcp/{name}` are the MCP plugin's domain (ADR-018, §mcp.endpoint).

## §host.wire — HTTP wire rules

Every `/api/*` response is a contract with the panel and with scripts that call it.

- **camelCase everywhere** (`#[serde(rename_all = "camelCase")]`).
- **Absent is not null.** The panel tests `undefined` in places (`bodyGone`, `childrenMb`,
  `measuredAt`): optional fields use `skip_serializing_if = "Option::is_none"`.
- **Numbers keep their shape.** `gatewayMb: 117.5` is a float with one decimal.
- **Error shapes differ by route on purpose.** MCP endpoints answer JSON-RPC
  (`{jsonrpc, error: {code: -32603, message}, id: null}`); admin routes answer
  `{error: "..."}`. Each refusal returns its route's own shape; do not unify them behind one
  axum error type.
- **Revisions.** A mutation of shared config names the `rev` it was planned against and fails
  with 409 on a mismatch, so two panels never silently overwrite each other (§host.config).

The golden capture of every `/api/*` response (asserting the shapes above wholesale) is still
open; today the shapes are pinned by the per-route tests.
