# swiss

<p><img src="assets/logo-wordmark.svg" alt="swiss" align="top" height="56"></p>

A developer's Swiss Army knife - one tiny local process, every tool behind one loopback port.

One local process that hosts every MCP server an AI client needs, exposed on HTTP paths under
`127.0.0.1:19999`. It began as a Rust port of the Node `local-mcp-gateway` (retired as the
reference on 2026-09-13, ADR-016 — this repository now owns every layer, the panel included:
plain ES modules under `crates/swiss-panel/src/admin_assets/`, tested by the vitest suite in
`crates/swiss-panel/panel-tests/`).

**Why the port exists:** memory. The Node build measures **113.8 MB RSS** on a typical workload
(re-measured 2026-09-11; the 2026-09-07 baseline read 117.5 MB) with 1×mysql, 1×pg, 2×redis,
2×http live and 2×proc asleep. Roughly 45 MB of that is the V8 floor, which no amount of tuning
in JavaScript can reach past. The same workload on this port reads **22.4 MB** (private bytes
8.6 MB), shipped as a **single self-contained `.exe`** with no Node, no `node_modules`, no npx
wrapper. Memory numbers are records, not gates.

## Status

**Implementation complete** — all planned adapter families are wired into the factory: echo,
MySQL, PostgreSQL, Redis, proc, HTTP, REST, and SSH tunnels. The admin API, embedded panel,
sealed-envelope compatibility, lazy proc lifecycle, and loopback security paths are implemented.
The build has since become the plugin toolbox `docs/09`–`12` describe: a plugin host over shared
Action / Run / process services, configuration-driven Jobs, a connection catalog that keeps Data
independent of MCP, and eight crates that still link into one `swiss.exe`.

Validation passes with default features — the full test suite across the whole workspace
plus Clippy with `-D warnings`. Run the gates with `--workspace`: without it cargo selects
the root package alone, runs only a small minority of the suite, and still reports ok.
Exact counts rot; `cargo test --workspace` is the only run that means "the suite passed".

`docs/01`–`08` describe the original compatibility port; `09`–`12` describe the toolbox it became;
`13`–`14` (panel navigation, the web terminal) are shipped.
**The number this project exists for is in.** `docs/01` records the full-workload side by side
(2026-09-11, `3c3fd7f`): Node **113.8 MB** vs Rust **22.4 MB** on the same data directory and the
same 60-call traffic — −80% RSS, −93% private bytes. What remains of the roadmap is the cutover
week (`docs/06` Phase 6), which is calendar, not code.

| Doc | What it settles |
| --- | --- |
| [`docs/01-goals-and-memory-budget.md`](docs/01-goals-and-memory-budget.md) | What "extreme memory thrift" actually buys, measured — and where it buys nothing |
| [`docs/02-architecture.md`](docs/02-architecture.md) | Crate layout, runtime model, module map |
| [`docs/03-dependency-map.md`](docs/03-dependency-map.md) | Every npm dependency → its crate, with feature flags |
| [`docs/04-porting-inventory.md`](docs/04-porting-inventory.md) | All 70 backend source files → destination, risk, phase |
| [`docs/05-wire-compatibility.md`](docs/05-wire-compatibility.md) | The on-disk and on-HTTP formats that MUST stay byte-identical |
| [`docs/06-roadmap.md`](docs/06-roadmap.md) | Six phases, each with an exit criterion |
| [`docs/07-decisions.md`](docs/07-decisions.md) | The calls that need a human: what gets dropped, and why |
| [`docs/08-testing.md`](docs/08-testing.md) | How 10,859 lines of vitest become the acceptance spec |
| [`docs/09-toolbox-plugin-architecture.md`](docs/09-toolbox-plugin-architecture.md) | RH-inspired developer toolbox: plugin/page contracts, module boundaries, staged migration. **P1–P6 shipped** |
| [`docs/10-config-driven-jobs.md`](docs/10-config-driven-jobs.md) | Configuration-driven Jobs: shared Actions/runs, policy defaults, compatibility and recovery. **Shipped** |
| [`docs/11-jobs-v2-implementation-spec.md`](docs/11-jobs-v2-implementation-spec.md) | The Jobs v2 schema, migration and scheduling semantics — stages S1–S6 with their acceptance tests. **Shipped; now the field contract of record** |
| [`docs/12-remaining-work-spec.md`](docs/12-remaining-work-spec.md) | The connection catalog, the touchstone plugin, the workspace split — and W2, the memory measurement, **still open** |
| [`docs/13-panel-navigation-spec.md`](docs/13-panel-navigation-spec.md) | Two-level panel navigation: pages grouped by the plugin that contributes them. **Shipped** |
| [`docs/14-terminal-plugin-spec.md`](docs/14-terminal-plugin-spec.md) | A web terminal plugin — remote SSH through a host capability, local PTY, xterm.js. **Shipped**; its local shell is off by default — turn it on from the gear beside the Terminal page's target picker |
| [`docs/15-terminal-paste-and-local-shell-spec.md`](docs/15-terminal-paste-and-local-shell-spec.md) | Terminal paste/copy keys the Windows way, and the local shell defaulting to pwsh with a panel switch. **Shipped** |
| [`docs/16-operations-hardening-spec.md`](docs/16-operations-hardening-spec.md) | Operations hardening: a daemon environment scrubbed of the launcher's agent/CI noise, an isolated 19998 test home, build stamps and a one-step deploy script, Rust CI, a RustCrypto duplicate-stack audit. **H1-H5 shipped** - H4 was already covered by the pre-existing build.yml; the audit is ADR-013 |
| [`docs/17-panel-design-canvas-spec.md`](docs/17-panel-design-canvas-spec.md) | The panel visual refresh as a design canvas: eight artboards, the Linear/Vercel restraint the panel aims for. **Direction only** — docs/18 shipped from its §2 decisions without waiting for the canvas |
| [`docs/18-panel-visual-refresh-spec.md`](docs/18-panel-visual-refresh-spec.md) | The panel visual refresh: tokens, the SVG sprite, the always-present page bar, left-aligned layout, one primary action + `⋯`, monochrome tags, one empty-state template. **V1–V7 shipped** (`810623a` on) |
| [`docs/19-secret-vault-spec.md`](docs/19-secret-vault-spec.md) | The device-bound secret vault: `secrets.json` under the same seal, `secret://name` references expanded at every use surface, values that go in and never come out. **Shipped**; ADR-014 |
| [`docs/20-groups-and-hierarchy-spec.md`](docs/20-groups-and-hierarchy-spec.md) | Groups everywhere: one `Groups` model in the host, one `/api/groups/{scope}` route family, one panel component with a visible hierarchy, and a Group field on every "new" — MCP, tunnels, jobs, secrets, tokens, the Data picker. **G1–G8 shipped** (`72dc010` on); ADR-015 |
| [`docs/21-data-web-gap-analysis.md`](docs/21-data-web-gap-analysis.md) | Data view vs five mature web DB tools (Adminer, DbGate, CloudBeaver, pgAdmin4, pgweb): the 35-item gap table with file:line evidence, read out of the read-only `../terminals-ref` clones |
| [`docs/22-data-parity-spec.md`](docs/22-data-parity-spec.md) | Data view full parity in six batches W0-W5: wiring-level exports/filters/timing, grid ergonomics, server-side completion, activity monitor, Redis structured editing, no-PK edits, streaming SQL dump, DDL minimal set. **All six batches W0-W5 shipped** (`6685d93` on, merged to master) |
| [`docs/24-mcp-path-domain-spec.md`](docs/24-mcp-path-domain-spec.md) | MCP endpoints move to `/mcp/<name>`: the root becomes host chrome plus future-plugin territory, the two host RESERVED lists retire, one panel URL builder changes. **P1–P5 shipped** (`7defb84` on; hard cutover, no alias, old-shape 404 carries a moved hint); ADR-018 |
| [`docs/25-vault-ref-envelope-spec.md`](docs/25-vault-ref-envelope-spec.md) | Vault references become `${secret://name}`: the `${...}` envelope gives token boundaries (a bare `secret://` inside a URL is literal text, vault state irrelevant), load-time whole-value migration with lazy disk writeback, in-envelope grammar hard-checked. **Items 1–5 shipped** (`7b49a82` on, merged to master); ADR-019 |
| [`docs/26-secrets-row-order-spec.md`](docs/26-secrets-row-order-spec.md) | Secrets rows join the drag family: the vault gains an `order` list (third table, same single rev-checked write), the scope's order route opens, rows drag within and across groups in one gesture. Supersedes docs/20's "secrets have no order" |
| [`docs/27-ssh-proxy-and-jump-spec.md`](docs/27-ssh-proxy-and-jump-spec.md) | SSH tunnels learn to dial through a proxy (hand-rolled HTTP CONNECT + SOCKS5, credentials as whole-field refs resolved at connect and handed to the handshake as components — never a secret-bearing URL) and through one another (`jump` = another connection's id, SSH-over-SSH like OpenSSH `-J`; cycles and proxy+jump rejected at save). **Spec — awaiting implementation** |
| [`docs/28-mcp-revisions-and-disable-spec.md`](docs/28-mcp-revisions-and-disable-spec.md) | MCPs gain same-name replacement with rollback (`revisions` = def snapshots parked per name, build-first transactional replace/restore, cap 5, never running), Stop is renamed Disable in panel wording (mechanism unchanged, clients already refused), and Rename/Disable/Delete reach the sidebar row's right-click menu (implementation note: a row is a `<button>`, so an ellipsis child would be invalid HTML — the data-grid ctx-menu anchor pattern is used instead). **Implementing on branch mcp**; ADR-023 |

Everything docs/09 and docs/10 designed is now code, and each document's status header names the
commit that landed it. None of it changed the Node-panel source-of-truth, the sealed formats, or
the v1 port boundaries below — the panel is still copied byte for byte, and a test fails if it
ever stops being.

## Deploying

`cargo build --release` makes the exe; `scripts/deploy.ps1` ships it: gates, stop,
build, `start --no-open`, then an assertion that the daemon on 19999 reports the hash this
build stamped into `swiss --version` (docs/16 H3). `-SkipGates` exists for hotfixes.
Deploying touches production — it is the operator's step, never part of iterating on a change
(live verification belongs to `scripts/test-instance.ps1` on 19998).

## The one-paragraph version

The admin panel (7,193 lines of dependency-free ES modules and CSS) ports **verbatim** — embedded in
the binary, byte for byte. Every on-disk format stays identical, so the Rust binary runs against the
same data directory as the Node build and the two can be A/B'd side by side on different ports. The
MCP protocol work is carried by `rmcp` 3.x, which implements the same 2026-07-28 revision and the
same stateless legacy fallback the Node gateway serves today. What remains is 15,395 lines of
backend TypeScript, ported in six phases, cheapest and highest-confidence first.

## Non-goals

- **Not a redesign.** Same features, same config, same panel, same HTTP surface. The only intended
  behavioural change is the one in [`docs/07-decisions.md`](docs/07-decisions.md) (ADR-001).
- **Not faster.** The Node build is not CPU-bound; it is idle almost all the time. Latency is
  dominated by the databases and child processes on the other end. Do not sell this as speed.
- **Not a saving on `proc` MCPs.** An `npx`/`uvx` child is 50–150 MB and stays exactly that.
