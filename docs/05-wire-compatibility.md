# 05 — Wire compatibility

This is the document that decides whether the port is a drop-in replacement or a migration event.

**The rule: the Rust build reads and writes the same bytes the Node build does.** Same data
directory, same file formats, same HTTP responses. Get this right and the two binaries can run side
by side on different ports against one data dir, which is what makes every other phase testable.
Get it wrong and the user's configuration has to be exported and re-imported by hand.

## The data directory

`~/.swiss` (overridable with `SWISS_HOME` / the legacy `MCP_GATEWAY_HOME`). Verified layout on this machine, 2026-09-07 (under the pre-rename name; the contents are unchanged):

```
~/.swiss/
  gateway.config.json      sealed   server layout, port, host, tokenEnv
  managed.json             sealed   user-added MCPs, overrides, toggles, enabled flags, tokens
  tunnels.json             sealed   SSH connections + forward rules
  env.json                 sealed   the credential store that replaced plaintext .env
  master.key               opaque   DPAPI-protected blob (Windows); not present on mac/Linux
  gateway-19999.pid                 pid file, port-scoped
  gateway-19999.log                 daemon log, port-scoped
  .proc-pids-19999.json             ledger of spawned proc-MCP PIDs, port-scoped
  logs/
    traffic.jsonl                   the traffic tail
    calls/
      <mcp>.jsonl                   one line per tool call: metadata + preview
      bodies/<mcp>/<seq>.txt        the full reply, when it exceeded the preview
    remote/                         the remote run log (docs/34 R9, 2026-09-18)
      runs.jsonl                    one line per finished remote run: the run row + input
      out/<runId>.txt               the run's whole output stream
```

Port-scoping is deliberate and load-bearing: two gateways on different ports must coexist, and a
shared PID ledger let one instance's reap kill the other instance's live children. Keep the naming.

> **Rename note:** the product is now named `swiss`; the on-disk wire surface is unchanged — the
> state file names, the sealed envelope (its `"lmg": 1` marker and `lmg-state-v1` HKDF info are
> frozen history a rename must not touch), and the config field names, including legacy
> `tokenEnv` values seeded with `MCP_GATEWAY_TOKEN`. Env overrides are read as `SWISS_HOME` /
> `SWISS_PORT` / `SWISS_TOKEN` / `SWISS_MASTER_KEY` first, with the `MCP_GATEWAY_*` names still
> honored, so state and shells written before the rename keep working.
>
> **The home directory did move** (2026-09-18): `~/.mcp-gateway` -> `~/.swiss`. The files inside
> are path-independent (DPAPI binds `master.key` to machine+user, not to a directory — the
> test-instance script has copied it into another home since docs/16), so the move is one
> `rename` of the whole directory, done by `swiss start` / `restart` and at `serve` boot
> (`swiss_core::paths::migrate_legacy_home`), refused while a `gateway-<port>.pid` in the old
> home names a live process. Until it happens, `data_dir()` keeps answering with the old
> directory, so every subcommand — `stop` included — still finds the running daemon's files.
> Nothing is ever written under the old name once `~/.swiss` holds files.

### The group keys (docs/20)

Every list the panel groups carries its one model as **additive** keys beside the data itself —
a file that predates groups names none of them and reads as the single `default` group with
every member unassigned:

| File | Keys | Notes |
| --- | --- | --- |
| `managed.json` | `groups` + `mcpGroups`; `tokenGroups` + `tokenMembers` | the MCP pair predates docs/20; the token pair is docs/20 G7, keyed by token id |
| `tunnels.json` | `connGroups` + `ruleGroups` | docs/20 lists `default` explicitly, marked by `tunnelGroupsV2: true` (the `groupsV2` maneuver: after the marker the list is literal, a deleted `default` stays deleted) |
| `secrets.json` | `groups` + `secretGroups` | values stay write-only — a group label is a folder name, not a credential |
| `gateway.config.json` | the jobs row's `groups` + each definition's sparse `group` | sparse: an absent `group` renders in the first group |

Because every key is additive, the Node-sealed fixtures keep passing unchanged — a Rust build
opens a file the Node build sealed before groups existed and answers `default` for every
member. No fixture needs regenerating, and the frozen-envelope assertion in this document is
unaffected.

The reverse direction is out of scope (docs/20 §1): a Node build reading a `tunnels.json` a
Rust build wrote will see `default` listed in `connGroups`. The two binaries already could not
share one home across a groups-era boundary, and narrowing that is not what docs/20 bought.

## 1. The sealed envelope — FROZEN

Every state file is AES-256-GCM under a machine-bound master key. The on-disk shape, confirmed
against the live `gateway.config.json`:

```json
{
  "lmg": 1,
  "alg": "aes-256-gcm",
  "keySource": "dpapi",
  "salt": "<base64, 16 bytes>",
  "iv":   "<base64, 12 bytes>",
  "tag":  "<base64, 16 bytes>",
  "ct":   "<base64>"
}
```

Exact construction, which the Rust implementation must reproduce bit for bit:

| Step | Value |
| --- | --- |
| Per-file key | `HKDF-SHA256(ikm = master, salt = salt, info = "lmg-state-v1", len = 32)` |
| Cipher | AES-256-GCM, 12-byte random IV, no AAD |
| Plaintext | the JSON payload, `JSON.stringify(data, null, 2)`, UTF-8 |
| Encoding | every binary field base64 |
| File itself | the envelope, `JSON.stringify(envelope, null, 2)`, written atomically (tmp + rename) |

Crates: `hkdf` + `sha2` + `aes-gcm`. Note that Node's `hkdfSync` takes the arguments in the order
`(digest, ikm, salt, info, len)` — the `salt`/`ikm` order is a classic place to get this silently
wrong, and "silently wrong" here means every existing file fails to open.

`keySource` is a **diagnostic only, never trusted**. Opening tries every candidate key the machine
can produce and lets the GCM tag decide. Port that behaviour, not a shortcut that reads `keySource`
and picks one.

### Legacy plaintext is accepted on read

A file that is not an envelope is read as plain JSON and then immediately re-sealed. This is how a
pre-encryption install migrates, and how a hand-authored `gateway.config.json` dropped into the data
dir works. If no key can be obtained at all, the plaintext is served read-only rather than failing
the boot. Keep both behaviours — they are the reason a user can hand-edit config in an emergency.

### The master key — this is the critical path

**Your current files are sealed with `keySource: "dpapi"`.** A Rust build that cannot do DPAPI
cannot read your configuration at all. Sources, in the order the Node build tries them:

| Platform | Source | Rust |
| --- | --- | --- |
| Windows | DPAPI (CurrentUser), blob stored as `master.key`, with `"local-mcp-gateway/v1"` as the entropy | `CryptUnprotectData` / `CryptProtectData` via the `windows` crate — **a direct call, replacing a PowerShell spawn** |
| macOS | login Keychain via the `security` CLI | keep the CLI, or `security-framework` |
| Linux | Secret Service via `secret-tool`, when a session daemon exists | keep the CLI |
| fallback | machine id (MachineGuid / IOPlatformUUID / `/etc/machine-id`), hashed | direct registry / file read |
| override | `MCP_GATEWAY_MASTER_KEY`, 64 hex chars | wins over everything; what the test suite uses |

The DPAPI entropy string `"local-mcp-gateway/v1"` is part of the format. Get it wrong and the blob
does not open.

**Do this in Phase 1 and prove it immediately:** a Rust unit test that decrypts a real envelope
sealed by the Node build (fixture sealed under `MCP_GATEWAY_MASTER_KEY`, so CI needs no OS keystore)
and a manual check that the binary opens the live `~/.mcp-gateway/gateway.config.json`. Until that
passes, nothing else in the port is worth writing.

## 2. The log formats

Append-only JSONL, read by seeking to the **end** of the file. Both are budget-trimmed by rewriting
the newest bytes to a temp file and renaming.

**`logs/calls/<mcp>.jsonl`** — one JSON object per line, fields exactly as `CallEntry`:
`seq, at, tool, via, client?, ok, ms, args, output, chars, preview?, body?, bodyGone?`.

Constants that are part of the format, not tuning knobs:

| Constant | Value | Meaning |
| --- | --- | --- |
| `ARGS_MAX` | 4 KB | arguments clipped inline |
| `PREVIEW_MAX` | 2 KB | reply head held inline, so a page needs no extra reads |
| `BODY_MAX` | 1 MB | ceiling on a stored reply |
| `BODY_KEEP` | 50 | full replies retained per MCP |
| `MAX_BYTES` / `KEEP_BYTES` | 2 MB / 1 MB | index budget and what a trim keeps |

`bodyGone` must stay **absent rather than false** while the payload is readable — there is a test
pinning exactly that, because the panel distinguishes the two. Body pruning is by **listing the
directory**, not by arithmetic on `seq`: sparse files (a run of replies that fit the preview and so
were never written) defeat the arithmetic. That bug has already been paid for once; the fix is in
the Node git log.

**`logs/traffic.jsonl`** — `TrafficEntry`, budgets 2 MB / 1 MB. See ADR-005 for the one intentional
change here: the resident 500-entry ring becomes disk-backed. The *file* format does not change.

## 3. The HTTP surface — frozen by the panel

The admin panel is copied into this repo unedited. That makes every `/api/*` response an interface
contract with 7,193 lines of JavaScript that will not be adapted to fit a Rust convention.

Practical consequences:

- **`camelCase` everywhere.** `#[serde(rename_all = "camelCase")]` on every response struct.
- **Absent is not null.** The panel checks `undefined` in places (`bodyGone` above, `childrenMb`,
  `measuredAt`). Use `#[serde(skip_serializing_if = "Option::is_none")]`, not `null`.
- **Numbers keep their shape.** `gatewayMb: 117.5` is a float with one decimal, not an integer.
- **Error shapes differ by route on purpose.** MCP endpoints answer JSON-RPC
  (`{jsonrpc, error: {code: -32603, message}, id: null}`); admin routes answer `{error: "..."}`.
  `Handler.refuse` returns the route's own shape. Do not unify them behind one axum error type.

The cheapest way to hold this line is a golden test: capture every `/api/*` response from the Node
build into `tests/golden/`, and assert the Rust build's responses parse to the same JSON value.
Write that harness in Phase 1 while the surface is still small.

## 4. Protocol compatibility

The gateway serves two protocol eras from one endpoint and must keep doing so:

- **2026-07-28** — the modern path, including the `server/discover` capability probe clients send
  before falling back to `initialize`. Stateless by default.
- **2025-era** — `initialize` and every request from a client that stayed on the old handshake,
  over the stateless legacy fallback.

In rmcp this is `StreamableHttpService` with `legacy_session_mode(false)`. There is deliberately **no
GET handler**: on 2026-07-28 notifications ride the client's `subscriptions/listen` POST stream, so
a GET has nothing to open and correctly falls through to 404. `DELETE /:path` acknowledges session
teardown with 204 even though there is no session — a client that sends it must be able to close
cleanly.

## 5. What is allowed to change

Only these, and each is an ADR in docs/07:

| Change | ADR |
| --- | --- |
| `"adapter": "./mod.mjs"` third-party module loading is dropped | ADR-001 |
| The traffic ring stops being resident (file format unchanged) | ADR-005 |
| The `--max-semi-space-size` / `--max-old-space-size` flags disappear with V8 | ADR-003 |

Anything else that changes on the wire is a bug in the port.
