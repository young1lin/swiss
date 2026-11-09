# §tunnels — The Tunnels plugin

Chapter 0007 of the [swiss specification](../SPEC.md).

SSH connections and the local port forwards that ride them: each rule forwards
`127.0.0.1:localPort` over SSH to `targetHost:targetPort`. Enabled rules stay up, reconnect
with backoff, and hold their local port **only while the tunnel can carry traffic**. A
connection can be dialled through an HTTP CONNECT or SOCKS5 proxy, or through another
connection as a jump host. The same connections are the terminal's remote hosts and the remote
plugin's build servers. Code: `crates/swiss-tunnels/src/tunnel/` (`types`, `store`, `ssh`,
`proxy`, `forward`, `manager`, `mcpmatch`, `port`, `import`, `shell`, `remote`, `api`), the
descriptor in `src/builtin.rs`, and the MCP glue in `src/mcp_link.rs`.

**Descriptor.** Id `tunnels`; pages `tunnels` ("SSH Connections", order 30) and
`tunnel-forwards` ("Port Forwards", 35), two sibling pages under one rail seat — `#tunnels`
keeps meaning SSH Connections; route `/api/tunnels`; no config rows; no `requires`.

**Start.** (1) On the first run (no `tunnels.json`) import a previous forward-port tool's
config from `%APPDATA%/forward-port/config.json`, every rule as `enabled:false` — the old tool
may still hold the ports; the import lives inside start, so a disabled plugin never touches the
file. (2) `manager.reopen()` lowers the close gate synchronously, so a Start click racing the
boot task is not refused. (3) A scoped task starts the enabled rules; each failure is a warning.
(4) Register the remote transport, then the shell provider, **last**; if either fails, nothing
is left registered.

**Stop.** The remote transport withdraws first (no drain — a remote exec is a run with its own
deadline and cancel, §remote); the shell provider withdraws and drains its sessions briefly,
naming how many were closed; `close_all` ends every forward and client (each `end()` capped at
1.5 s) and releases every port; the providers are cleared. **The enabled flags are untouched**,
so the next start restores exactly the set that was running.

**Ordering.** Tunnels starts before the MCP plugin and stops after it, so an MCP that rides a
tunnel finds it up (§host.lifecycle).

## §tunnels.api — The routes

The tree mounts inside the host's loopback guard and plugin boundary; the boundary is the gate.
Errors funnel through one function: a guard conflict is **409**
`{error, dependents, confirmRequired:true}`; a message starting with `unknown ` is 404;
anything else is 400. Request bodies coerce like JavaScript's `Number()`; `?force=1` and
`{force:true}` are both accepted.

| Route | Answer |
|---|---|
| `GET /api/tunnels` | `{connections, rules, ruleGroups, connGroups, mcps}` — the live rows plus groups and MCP names |
| `GET /keys` | `{keys:[{path,name}], defaultPath}` — `id_*`, `*.pem`, `*.key` under `~/.ssh` |
| `GET /browse?dir=` | `{dir, parent?, entries:[{name,path,dir}], error?}` — directories first; a read failure is the `error` field |
| `GET /suggest/{port}` | `{port, mcps:[name]}` (§tunnels.mcp) |
| `POST /connections`, `PUT /connections/{id}`, `DELETE /connections/{id}` | 201 `{connection}` (masked); `{connection}`; `{id, deleted:true}` — a connection still carrying rules or serving as a jump host is refused, naming them |
| `POST /connections/{id}/test` | `{ok, ms, banner?, error?, kind?, fingerprint?, hostKey?}` |
| `POST /connections/{id}/trust` | `{id, hostKey}` — accept the presented key, or clear it to relearn |
| `POST /rules`, `PUT /rules/{id}`, `DELETE /rules/{id}` | 201 `{rule}` (`start:true` starts it; a failed start does not undo the save); `enabled` is not accepted from the client |
| `POST /rules/{id}/start`, `/stop` | `{rule}`; a failed start is **a result, not a transport error**: 200 `{rule, ok:false, error}` |
| `POST /start-all`, `/stop-all` | `{results:[{id,name,ok,error?}]}` — started concurrently, grouped by connection |
| `GET /port/{port}` | `{port, free, owner|null}` — a real bind probe, the holder from `netstat -ano` + `tasklist` (about 4 MB and 40 ms, against PowerShell's 65 MB and 350 ms) |
| `POST /port/{port}/free` | `{port, killed:{pid,name}}` — `taskkill /F`; refuses pid 0 and the gateway itself; Windows only |

Grouping and order go through the host's one groups family (`/api/groups/{conns,rules}`,
§host.groups).

## §tunnels.store — Connections, rules and tunnels.json

`tunnels.json` is a sealed state file (§core.sealed):
`{connections, rules, ruleGroups, connGroups}`. **Key order is the file format** —
serialisation is hand-written, camelCase, absent rather than null.

- **Connection:** `id, name, host, port, username, authType` (`key`|`password`), then the
  optional `keyPath, passphrase, password, hostKey, group`, then the proxy and jump fields
  **appended** in the order `proxy, proxyUsername, proxyPassword, jump`. An unset field keeps
  the old key set exactly; a test pins both halves.
- **Rule:** `id, name, connectionId, localPort, targetHost, targetPort, remark,
  autoReconnect, reconnectInterval, enabled, mcps`, then `group`.
- **Validation** (`valid_conn`, `valid_rule`): ports are integers 1–65535; the gateway's own
  port is refused as a local port; ids are unique; local ports do not collide;
  `reconnectInterval` NaN or 0 becomes 10, with a floor of 1.
- **Proxy and jump validation** (English messages):
  - `proxy` is `http://` or `socks5://` `host[:port]`; a portless URL is saved normalised
    (http → 80, socks5 → 1080). `socks5h://` and `https://` get their own messages (swiss always
    lets the proxy resolve names; TLS to the proxy is not built). userinfo, path and query are
    refused: `proxy URL must not carry credentials; use the proxy username and password fields`.
  - `jump` must name another existing connection; itself is refused, and a cycle is refused
    naming the chain: `jump cycle detected: A -> B -> A`.
  - A connection may have a proxy **or** a jump, not both: `put the proxy on the jump
    connection if it needs one`.
- **Tolerant load.** A corrupt file is a warning and empty tables. A row with an empty name or
  host or a bad local port is dropped, but **a rule with an unknown connection is kept** and
  shown as an error — one bad hand edit never eats the other rows.
- **Groups.** `default` is an ordinary, explicitly listed name (a pre-groups file has it
  materialised); rename migrates members; deleting a group moves its members to the **first**
  group; order is array order (§host.groups).
- **Host keys.** An edit that keeps host and port re-adopts the stored key, so editing never
  silently drops trust; `trust` and the clear path are explicit.

**Credentials.** `password`, `passphrase`, `proxyUsername` and `proxyPassword` hold a literal
or a whole-field reference (`${ENV}` or `${secret://name}`, §host.vault); the reference is what
lands on disk, and it resolves **at connect time**, strictly — a missing reference refuses the
connection before any socket opens, naming the reference. The API masks secrets outbound and
restores the sentinel inbound (`mask_conn`/`unmask_conn`), so editing an unrelated field never
destroys a stored secret; `password`, `passphrase` and `proxyPassword` ride the rows as the sentinel unless each is a
reference, which is not a secret and passes as itself. The rows also carry `keyPath`, so a save
never rewrites a custom path to the default.

A connection Test uses a throwaway client and persists no host key. Its optional `fingerprint`
reports the key presented by an unpinned server or the actual key on a mismatch; a matching stored
key leaves it absent. This lets `setup apply` ask for confirmation without pinning it (§host.setup).

## §tunnels.ssh — The SSH client

- **One client per connection, reference-counted** (`acquire`/`release`): 14 rules on one host
  dial once; forwards, terminal sessions and remote execs share it; `end` happens when the last
  user leaves. Dialling is single-flight.
- **Budget.** TCP + key exchange + auth (and any proxy handshake or jump chain) fit in one 15 s
  budget (`READY_TIMEOUT`). Keepalive 15 s × 3 detects a NAT-dropped link in about 45 s; a
  watcher per client reports the loss with a generation, so a stale report is ignored. A
  session that answers keepalives but stops answering channel opens is invisible here —
  §tunnels.selfheal judges it from the forward layer.
- **Auth.** `key` (a private key file, `~` expanded, an optional passphrase) or `password`.
- **Host keys are TOFU.** The fingerprint (`SHA256:…`, OpenSSH format) is learned on the first
  connect; a changed key is refused with the expected and actual fingerprints, and the panel
  offers Trust. A Test on a connection without a stored key stores nothing.
- **Test** is a throwaway handshake (`{ok, ms, banner, …}`) through the same dial path.
- **Failure kinds** (`FailureKind`): `auth`, `hostkey`, `config` are **never retried** (retrying
  a bad password invites fail2ban); `network` and `port` are. auth and hostkey put the rule in
  `error`, not `reconnecting`.
- The PTY requests `TERM=xterm-256color` (§terminal).

## §tunnels.forward — Forward rules

- **Start** binds the local port, acquires the client and runs the accept loop (at most 200
  sockets per rule); each socket is pumped to a `direct-tcpip` channel with byte counts; TCP
  keepalive 30 s, and the channel open itself waits at most 10 s (§tunnels.selfheal). A
  successful start persists `enabled:true`.
- **Stop** closes the listener and every socket and verifies the port can be bound again;
  `persist` decides whether `enabled` is cleared.
- **Per-rule serialisation.** Every operation on a rule holds that rule's async mutex for the
  whole operation; run state is snapshotted under a short lock, and **no lock is held across an
  await**.
- **Loss and recovery.** When a client dies, or is judged wedged (§tunnels.selfheal), each
  affected rule **releases its port first, then reports** `reconnecting` with the reason;
  recovery runs through each rule's own queue; a user stop racing it wins (re-checked under
  the lock).
- **Backoff.** `interval × 2^(n−1)` with the exponent capped at 8, capped at 5 minutes,
  ±20 % jitter; a success resets the count. Retry timers hold weak references and are
  abortable.
- **Edits.** Editing a connection stops its running rules, swaps the client and restarts only
  those that were running; editing a connection used as a jump host reconnects its dependants.
- **A stale listener** of another rule in this gateway holding the port is torn down through
  that rule's own queue, and the start is retried once. A foreign holder is reported
  (`portOwner`), and the panel offers **Force free**.
- **Rows** carry `sockets`, `bytesIn`, `bytesOut`, `channelFailures`, `lastError`, and the dot
  state: connected/up green, connecting/starting/reconnecting yellow, error red, idle/stopped
  grey.

## §tunnels.selfheal — A wedged session

Keepalive (§tunnels.ssh) detects a dead transport; a session can also die silently above it:
sshd keeps answering keepalives while every `direct-tcpip` open hangs — the local port stays
bound, the rule stays green, and the MCPs behind it time out their pools. The forward layer
is the only probe that sees this, so it owns the judgment.

- **The open budget.** A `direct-tcpip` open that no answer reaches in 10 s fails: the
  accepted socket is closed (the client errors fast instead of hanging on its pool
  timeout), the rule's `channelFailures` counts it, and `lastError` names it. An answered
  refusal keeps today's meaning — the session proved it is alive, the target is what said
  no. The data path stays timeout-free: an idle pooled socket is legitimate and is never
  probed.
- **The wedge judgment.** Three opens on one connection timing out consecutively — with
  no answered open in between — judge the session wedged: the client is ended, and every
  rule on it runs the ordinary loss path (§tunnels.forward) — port released first,
  `reconnecting` with the reason, re-dial under the existing backoff, linked MCP pools
  dropped on recovery. A rule with `autoReconnect` off lands in `error`, exactly as a
  transport loss does.
- **Not a target probe.** The judgment is connection-wide and resets on any answered
  open, so one unreachable target (its opens time out while a sibling rule's opens
  succeed) fails fast without tearing down healthy neighbours; re-dial churn while a
  target is down is bounded by the existing backoff.

## §tunnels.proxy — Proxy dialling

`proxy.rs` dials the proxy itself and hands the stream to russh's `connect_stream`; no new
dependency.

- **Order.** Resolve the credential references first (a failure is `config`, no dial, the
  reference named); each resolved value is at most 255 bytes (RFC 1929). Then connect to the
  proxy with `TCP_NODELAY`.
- **HTTP CONNECT.** `CONNECT host:port HTTP/1.1`, `Host:`, and `Proxy-Authorization: Basic`
  when credentials are set; a non-2xx status is `proxy <h>:<p> refused CONNECT: HTTP <code>`.
  The response head is read under an 8 KB cap.
- **SOCKS5.** Offer no-auth, plus username/password when set; the RFC 1929 sub-negotiation must
  answer `[01 00]`. CONNECT sends the **target name unresolved** (ATYP 03 — socks5h
  semantics; an IP literal uses ATYP 01/04). A non-zero reply is `socks5 reply <rep>`.
- **Credentials reach the handshake only as components.** No code path builds a URL containing
  a password; the dialler is the only consumer and does not take URLs.
- **Every proxy failure is constructed as `network`** — never classified from its message — so
  it is retryable and auto-reconnect applies.

Not built: ssh-agent auth, `-D` dynamic forwarding, `ProxyCommand` and `~/.ssh/config`
(never: a subprocess and an unbounded grammar), TLS to the proxy, SOCKS4, proxy chains, per-hop
timeouts, and socks5 for the MCP http adapters.

## §tunnels.jump — Jump hosts

`jump` is another connection's id, and the transport is recursive — OpenSSH `-J`'s per-hop
model:

```
transport(conn) = conn.jump ? live(jump).open_channel(conn.host, conn.port)   // SSH over SSH
                            : conn.proxy ? proxy::dial(conn) : TcpStream
```

- Each hop is an ordinary connection with its own auth, TOFU, Test and row; the target's
  `hostKey` is the final server's. A proxy for a hop lives on that hop.
- **Holds.** Each connection holds its direct jump host; stopping the target returns every
  hop's reference count to zero (a test pins it on a three-hop chain). Test builds a private
  throwaway chain.
- **Failures.** A dead jump kills the target's channel; the target reports `network`, releases
  its ports, and reconnects by re-evaluating the whole chain. A hop's auth or hostkey failure
  keeps its kind and is prefixed `via <jump-name>: …`.
- Deleting a connection used as a jump host is refused, listing its dependants; cycles are
  refused at save time, so the runtime never sees one.

## §tunnels.mcp — MCPs that ride a tunnel

- **Suggest.** `GET /suggest/{port}` is pure definition analysis (`mcpmatch.rs`): mysql and
  redis read host/port (defaults 3306/6379), pg parses its connection string; references are
  resolved first — an unresolvable one does not match. proc and echo never match.
- **Links.** A rule's `mcps` names the MCPs it serves; rows carry `mcpRows`
  (`{name,state,known}`). When a tunnel reconnects after a linked MCP started, that MCP's
  pooled sockets (MySQL, PostgreSQL, Redis, MongoDB) are dropped so the next call dials
  the new session. `stalePool` stays set only when the adapter cannot drop them — a child
  process has no pool to release — and the detail page then still points at Restart.
  The MCP detail page lists its tunnels.
- **The guard.** Stopping or deleting a rule that a **started** MCP depends on, or stop-all, is
  409 `Dependents`; the panel lists them and resends with force.
- **Renames.** An MCP rename migrates the links and a delete clears them.
- **No dependency cycle.** swiss-tunnels defines two narrow traits (`McpView`, `McpDisplay`);
  the registry-backed implementations live in the composition layer (`src/mcp_link.rs`), so
  neither plugin crate depends on the other (§arch).

## §tunnels.providers — Shells and remote execution

The tunnels connections are the **one** host inventory. `TunnelShells` implements the host's
shell capability for the terminal (§terminal): PTY sessions take a reference on the shared
client, and credentials never leave `ssh.rs`. `TunnelRemote` implements the remote transport
for the remote plugin (§remote): exec requests over the same clients, POSIX-quoted here where
the SSH string is built, returning events and exit codes only. It calls the seat's `changed()`
(§host.seats) on every connection state transition — idle, connecting, connected, error — and
on every connection added, edited or deleted, so the Remote page's endpoint dots follow without
a poll.

## §tunnels.panel — The pages

- **Two pages over one scope switch** (`mountTunnelsPage("conns" | "rules")`), grouped cards
  with collapse state, drag to reorder and regroup through the groups family; a poll patches
  dots, reasons, buttons and counts in place and redraws only when the row set changes, never
  while dragging.
- **Rule rows**: an inline Start/Stop, ⋯ with Edit, Copy local port, Force free (only with a
  `portOwner`), Delete; a 409 lists the dependants and confirms before the forced resend.
- **Connection rows**: Test (a hostkey failure offers Trust, then retries); ⋯ Edit, Copy host,
  Delete. Badges: `proxy`, `via <name>`.
- **The connection sheet**: key auth shows the key path (prefilled from the row) with Browse…
  and the passphrase; password auth swaps in a password. An **Advanced** `<details>`, closed by
  default, holds Proxy (URL, username, password) and Via connection (a jump picker excluding
  itself); its summary carries `proxy` / `via <name>` chips when set, so closed never means
  hidden. The backend is the one judge of cycles; its 400 shows inline.
- **The rule sheet**: changing the local port asks `/suggest` and pre-checks the matching MCPs;
  `mcps` is sent only when the panel means it (omitted keeps, `[]` clears).
