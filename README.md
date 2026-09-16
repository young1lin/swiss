# swiss

<p><img src="assets/logo-wordmark.svg" alt="swiss" align="top" height="56"></p>

A developer's Swiss Army knife — one tiny local process, every tool behind one loopback port.

swiss serves on `127.0.0.1:19999`: every MCP server an AI client needs, on HTTP paths under
`/mcp/<name>`, plus database browsing, SSH tunnels, scheduled jobs and a web terminal — one
static binary, one admin panel, no script runtime, no `node_modules`, no npx wrapper. It began
as a rewrite of an earlier Node.js gateway (retired; ADR-016) and now owns every layer, the
panel included: plain ES modules under `crates/swiss-panel/src/admin_assets/`, served straight
from the binary with no bundler and no build step.

**Why it exists:** memory. The old build measured 113.8 MB RSS on a typical workload; the same
workload here reads **22.4 MB** (private bytes 8.6 MB), shipped as a single self-contained
executable. Memory numbers are records, not gates — [`docs/01`](docs/01-goals-and-memory-budget.md) holds them.

## Install

Download the binary for your OS from
[GitHub Releases](https://github.com/young1lin/swiss/releases) and put it on your PATH:

| Asset | OS |
| --- | --- |
| `swiss-<version>-x86_64-pc-windows-msvc.exe` | Windows x64 |
| `swiss-<version>-x86_64-unknown-linux-gnu` | Linux x64 |
| `swiss-<version>-aarch64-unknown-linux-gnu` | Linux arm64 |
| `swiss-<version>-aarch64-apple-darwin` | macOS (Apple Silicon) |
| `swiss-<version>-x86_64-apple-darwin` | macOS (Intel) |

```
swiss start            # start the gateway on 127.0.0.1:19999 and open the panel
swiss token            # the bearer token an MCP client authenticates with
swiss creds            # panel URL + token, ready to paste into a client
swiss skill install    # the shipped skill, for AI agents that drive swiss
```

Or build from source: Rust stable, `cargo build --release` — the same single binary.

Loopback-only is security, not a default: the gateway binds `127.0.0.1`, refuses every
non-loopback `Host`/origin, and refuses a non-loopback `host` in config at load. Reach it
remotely by forwarding the port over SSH, never by widening the bind.

## Update

`swiss update` compares the running build with the newest GitHub release and prints the steps.
The update itself stays a manual swap, on purpose — a downloader inside a resident process is
the opposite of the memory budget this project exists for.

```
swiss update     # "up to date", or the newest release and its download link
swiss stop
# replace swiss(.exe) with the downloaded binary
swiss start
```

Nothing migrates during an update: all state lives in sealed files under the swiss home
directory, never inside the binary. Moving machines uses `swiss export > bundle.json` and
`swiss import bundle.json` — sealed files are bound to the machine that sealed them.

## Start at sign-in

The panel's Settings → Plugins page carries a "Start swiss when you sign in" switch, or from
the terminal:

```
swiss autostart on     # Windows: an HKCU Run value · macOS: a LaunchAgent · Linux: a systemd user unit
swiss autostart off
```

## Status and validation

**Implementation complete.** Every planned adapter family is wired into the factory: echo,
MySQL, PostgreSQL, Redis, proc, HTTP, REST, zai-vision, and SSH tunnels. The admin API, the
embedded panel, sealed-envelope compatibility, the lazy proc lifecycle and loopback security
paths are all in, and the build is the plugin toolbox `docs/09`–`12` describe: a plugin host
over shared Action/Run/process services, configuration-driven Jobs, and eight crates that
still link into one `swiss` binary.

The gates: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
and the vitest suite in `crates/swiss-panel/panel-tests/`. `--workspace` is load-bearing:
without it cargo selects the root package alone, runs a small minority of the suite, and
still reports ok.

## Documentation

| Doc | What it settles |
| --- | --- |
| [`docs/01-goals-and-memory-budget.md`](docs/01-goals-and-memory-budget.md) | What "extreme memory thrift" actually buys, measured — and where it buys nothing |
| [`docs/02-architecture.md`](docs/02-architecture.md) | Crate layout, runtime model, module map |
| [`docs/03-dependency-map.md`](docs/03-dependency-map.md) | Every npm dependency → its crate, with feature flags |
| [`docs/04-porting-inventory.md`](docs/04-porting-inventory.md) | All 70 backend source files → destination, risk, phase |
| [`docs/05-wire-compatibility.md`](docs/05-wire-compatibility.md) | The on-disk and on-HTTP formats that MUST stay byte-identical |
| [`docs/06-roadmap.md`](docs/06-roadmap.md) | Six phases, each with an exit criterion |
| [`docs/07-decisions.md`](docs/07-decisions.md) | The calls that need a human: what gets dropped, and why |
| [`docs/08-testing.md`](docs/08-testing.md) | How the vitest suite becomes the acceptance spec |
| [`docs/09-toolbox-plugin-architecture.md`](docs/09-toolbox-plugin-architecture.md) | The developer toolbox: plugin/page contracts, module boundaries, staged migration. **P1–P6 shipped** |
| [`docs/10-config-driven-jobs.md`](docs/10-config-driven-jobs.md) | Configuration-driven Jobs: shared Actions/runs, policy defaults, compatibility and recovery. **Shipped** |
| [`docs/11-jobs-v2-implementation-spec.md`](docs/11-jobs-v2-implementation-spec.md) | The Jobs v2 schema, migration and scheduling semantics — stages S1–S6 with their acceptance tests. **Shipped; now the field contract of record** |
| [`docs/12-remaining-work-spec.md`](docs/12-remaining-work-spec.md) | The connection catalog, the touchstone plugin, the workspace split — and W2, the memory measurement, **still open** |
| [`docs/13-panel-navigation-spec.md`](docs/13-panel-navigation-spec.md) | Two-level panel navigation: pages grouped by the plugin that contributes them. **Shipped** |
| [`docs/14-terminal-plugin-spec.md`](docs/14-terminal-plugin-spec.md) | A web terminal plugin — remote SSH through a host capability, local PTY, xterm.js. **Shipped**; its local shell is off by default — turn it on from the gear beside the Terminal page's target picker |
| [`docs/15-terminal-paste-and-local-shell-spec.md`](docs/15-terminal-paste-and-local-shell-spec.md) | Terminal paste/copy keys the Windows way, and the local shell defaulting to pwsh with a panel switch. **Shipped** |
| [`docs/16-operations-hardening-spec.md`](docs/16-operations-hardening-spec.md) | Operations hardening: a daemon environment scrubbed of the launcher's agent/CI noise, an isolated 19998 test home, build stamps and a one-step deploy script, Rust CI (`.github/workflows/build.yml`), a RustCrypto duplicate-stack audit. **H1–H5 shipped**; the audit is ADR-013 |
| [`docs/17-panel-design-canvas-spec.md`](docs/17-panel-design-canvas-spec.md) | The panel visual refresh as a design canvas: eight artboards, the Linear/Vercel restraint the panel aims for. **Direction only** — docs/18 shipped from its §2 decisions without waiting for the canvas |
| [`docs/18-panel-visual-refresh-spec.md`](docs/18-panel-visual-refresh-spec.md) | The panel visual refresh: tokens, the SVG sprite, the always-present page bar, left-aligned layout, one primary action + `⋯`, monochrome tags, one empty-state template. **V1–V7 shipped** |
| [`docs/19-secret-vault-spec.md`](docs/19-secret-vault-spec.md) | The device-bound secret vault: `secrets.json` under the same seal, `secret://name` references expanded at every use surface, values that go in and never come out. **Shipped**; ADR-014 |
| [`docs/20-groups-and-hierarchy-spec.md`](docs/20-groups-and-hierarchy-spec.md) | Groups everywhere: one `Groups` model in the host, one `/api/groups/{scope}` route family, one panel component with a visible hierarchy, and a Group field on every "new". **G1–G8 shipped**; ADR-015 |
| [`docs/21-data-web-gap-analysis.md`](docs/21-data-web-gap-analysis.md) | Data view vs five mature web DB tools (Adminer, DbGate, CloudBeaver, pgAdmin4, pgweb): the 35-item gap table with file:line evidence |
| [`docs/22-data-parity-spec.md`](docs/22-data-parity-spec.md) | Data view full parity in six batches W0–W5: wiring-level exports/filters/timing, grid ergonomics, server-side completion, activity monitor, Redis structured editing, no-PK edits, streaming SQL dump, DDL minimal set. **All six batches shipped** |
| [`docs/24-mcp-path-domain-spec.md`](docs/24-mcp-path-domain-spec.md) | MCP endpoints move to `/mcp/<name>`: the root becomes host chrome plus future-plugin territory. **P1–P5 shipped** (hard cutover, no alias, old-shape 404 carries a moved hint); ADR-018 |
| [`docs/25-vault-ref-envelope-spec.md`](docs/25-vault-ref-envelope-spec.md) | Vault references become `${secret://name}`: the `${...}` envelope gives token boundaries (a bare `secret://` inside a URL is literal text), load-time whole-value migration with lazy disk writeback, in-envelope grammar hard-checked. **Items 1–5 shipped**; ADR-019 |
| [`docs/26-secrets-row-order-spec.md`](docs/26-secrets-row-order-spec.md) | Secrets rows join the drag family: the vault gains an `order` list (third table, same single rev-checked write), the scope's order route opens, rows drag within and across groups in one gesture. Supersedes docs/20's "secrets have no order" |
| [`docs/27-ssh-proxy-and-jump-spec.md`](docs/27-ssh-proxy-and-jump-spec.md) | SSH tunnels learn to dial through a proxy (hand-rolled HTTP CONNECT + SOCKS5, credentials as whole-field refs resolved at connect) and through one another (`jump` = another connection's id, SSH-over-SSH like OpenSSH `-J`; cycles and proxy+jump rejected at save). **Spec — awaiting implementation** |

Everything docs/09 and docs/10 designed is now code, and each document's status header names
the commit that landed it. None of it changed the sealed formats — wire compatibility is
docs/05's to guard, and its fixture is committed under `tests/`.

## Development

Two instances, two ports: **19999 is the operator's production instance**, and **19998 is where
every change is verified** — `scripts/test-instance.ps1` snapshots state into an isolated test
home and serves the fresh build there; panel changes additionally walk the page in a real
browser. Deploying to 19999 is the last step, done once, through `scripts/deploy.ps1` (gates
→ stop → build → start → prove the served build hash equals the freshly built binary).

## Non-goals

- **Never remote.** Loopback-only is the boundary; forward over SSH instead of widening it.
- **No self-updating binary.** `swiss update` checks and instructs; the swap is yours.
- **No saving on `proc` MCPs.** An `npx`/`uvx` child is 50–150 MB and stays exactly that.

## License

Apache License 2.0 — see [LICENSE](LICENSE). The panel vendors third-party pieces under
`crates/swiss-panel/src/admin_assets/js/vendor/`, each carrying its own notice: xterm.js and
its addons (MIT), cronstrue (MIT).
