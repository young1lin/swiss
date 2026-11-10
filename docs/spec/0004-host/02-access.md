# Access, config and credentials

Part 02 of [§host](README.md), chapter 0004 of the [swiss specification](../../SPEC.md).

## §host.boundary — The loopback boundary

swiss is a local tool. Three checks run on every route, before anything else:

1. **Bind.** The configured host must be a loopback address; a non-loopback host is refused
   when the config loads, not warned about.
2. **Peer.** The peer address must be loopback (an IPv4-mapped `::ffff:` prefix is stripped).
3. **Host and Origin.** Both headers, when present, must name loopback — the DNS-rebinding
   defence.

A refusal is `403 {error}`. Requests without `ConnectInfo` (tests driving the router with
`oneshot`) count as local; a real listener always uses `into_make_service_with_connect_info`.

**No framing.** Every answer — the panel shell, the lock page, assets, API answers and refusals —
carries `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY`. The panel
frames nothing of its own, and a page on another local port that framed the signed-in panel
could steer its clicks.

## §host.session — The admin session

The loopback checks cannot tell one local process from another, so `/api/*` and the panel
also need a credential. Two open the admin surface, **on top of** the loopback checks:

| Part | Rule |
|---|---|
| Login token | single-use, 120 s (`TICKET_TTL_MS`), at most 16 unredeemed. Minted by `swiss start` (which then opens the browser) and `swiss open`, via `POST /api/session/ticket` → `{token, url}` (itself protected) |
| Redemption | only `GET /?token=<t>`: valid → 303 to `/` with the cookie; invalid, used or expired → 401 lock page |
| Browser session | cookie `swiss_session_<port>` (the port is in the name: 19998 and 19999 share `127.0.0.1`), value `v1.<issued ms>.<random>.<HMAC-SHA256>`, `HttpOnly; SameSite=Strict; Path=/; Max-Age` 30 days |
| Cookie provenance | the cookie opens `/api/*` only for the panel's own pages: `Sec-Fetch-Site` must be `same-origin` or `none` when the browser sends it, else `Origin` must name the request's own `Host`, port included; a write with neither is refused (403). `SameSite=Strict` alone would let a page on any other port of `127.0.0.1` ride the cookie into blind POSTs. The CLI key needs no provenance: no page can send it |
| Signing key | 32 bytes in the sealed `session.json`, kept across restarts so a signed-in browser stays signed in |
| CLI key | `cliKey` in the same file, rotated on every daemon start, sent as `X-Swiss-Key` (never `Authorization`, which carries MCP bearers). Only the same OS user on the same machine can unseal it |
| Protected | every `/api/*` in both trees, the terminal WebSocket upgrade, and `GET /api/bus`. `/` without a session serves the lock page; `/admin/*` and `/health` carry no data and stay open; `/mcp/*` keeps its bearer tokens. The bus upgrade also requires `Origin` to equal `Host`, port included; a missing Origin passes only when the CLI key opened the request (SPEC §bus.security) |
| Fail closed | a request with `ConnectInfo` arriving while the session is not wired answers 401 |

The panel's `api()` answers a 401 with `location.assign("/")`; the lock page (English and
Chinese) says one thing: run `swiss open`. No username/password and no logout — deleting
`session.json` regenerates the key and ends every session.

## §host.tokens — MCP client tokens

`/mcp/*` is authenticated by bearer tokens, checked before the body is read.

- **Named tokens** — one per client, individually revocable: `GET/POST /api/tokens`,
  `GET /api/tokens/{id}/secret`, `POST /api/tokens/{id}/rotate`, `DELETE /api/tokens/{id}`.
  Comparison is timing-safe; `verify` returns the matching record so the call log and traffic
  attribute each request. A token's secret can be read back (loopback-only, behind the admin
  session): unlike the vault, a token is compared in plaintext anyway.
- **The config token** — the variable named by `tokenEnv` (overlay, then process env), then
  `SWISS_TOKEN`. The first boot seeds a token (`random_hex(24)`) when none exists.
- Token groups are the `tokens` scope (§host.groups); tokens have no manual order.
- Create, rotate, remove and a `tokens` group edit publish HOST.TOKENS (module 4, sub 4, an
  empty `Notice`). A refused write publishes nothing. The shell re-reads `GET /api/tokens`;
  the Tokens page paints from that read and has no poll of its own. `/api/info` is read once
  at boot and after each WELCOME (§bus.link).

## §host.config — Config and the config store

`gateway.config.json` holds `port`, `host`, `tokenEnv`, `servers` (config-sourced MCPs),
root-level subsystem rows and versioned `plugins.<id>` rows `{kind?, disabled?, config?}`.

- **One grammar.** A missing row means enabled; only a literal `disabled: true` disables
  ("turn off jobs" = `{"jobs": {"disabled": true}}`). Unknown fields in a known schema are
  refused with their path.
- **The v1 → v2 projection does not migrate.** Old root rows keep working in place; a
  `plugins.<id>` row takes precedence (an empty one counts). Boot never rewrites the file; only
  an operator change does.
- **Revision** = the top 53 bits of the content's SHA-256 — fits a JS number, stable across
  restarts, and no metadata is written into the file. Every write is compare-and-set: a stale
  revision is 409 "configuration changed; reload before saving".
- **Validation layers.** PUT runs `validate_config` (rejected values are not persisted);
  boot runs `validate_config_for_start` (jobs drops bad definitions left by an older
  validator with a warning instead of failing the plugin); `warnings` report "storable but not
  runnable now" (an unregistered action). A generic guarantee: every plugin refuses a key it does
  not read on a save (`every_plugin_with_a_schema_refuses_an_unknown_key`), the ones whose row holds
  no settings included, because a key saved and ignored is a setting the user believes in and does
  not have. A boot reads past such a key, on `mcp`, `tunnels`, `data` and `terminal`, so one an older
  build saved cannot stop a start.
- **Port.** `SWISS_PORT` > the config > 19999. `--port` is saved into the config (sealed,
  atomic) as the new default; illegal values are refused ("takes a number between 1 and
  65535").
- `managed.json` holds user-added MCPs, overrides, the enabled map, named tokens, the sidebar
  order, groups and parked revisions (§mcp.revisions).

## §host.refs — Credentials and the resolver

Credentials in config are references, never literals: `${ENV_VAR}` or `${secret://name}`.
Literal values exist only inside sealed files and are masked when echoed.

**One resolver, one pass** (`swiss_core::secure::refs::resolve`), applied at every use point
— there is no allowlist of boundaries, it is a contract every plugin inherits:

- `${UPPER_SNAKE}` → the env overlay, then the process env. **Missing → empty string** (the
  machine's current state may legitimately lack it). `process.exec` is the strict exception: a
  missing variable is a named error.
- `${secret://kebab-name}` → the vault. **Missing → hard failure** naming the use point and the
  reference, never a value (`references secret://stripe-key which is not in the vault`). An
  operator declared it; an empty credential would become a mysterious 401 on someone else's
  server.
- `${secret://name:default}` → the text after the first `:` stands in when the vault lacks the
  name.
- Outside `${…}` there are no references: a bare `secret://` is literal text, so
  `https://x/secret://aaa/y` passes byte-identical. Invalid names inside the envelope
  (`${secret://BadName}`) fail hard. An unclosed `${` is literal. A replaced value is never
  re-scanned. The scanner is hand-written over characters (ADR-007).
- Legacy whole-value `secret://name` strings are rewritten to `${secret://name}` at load
  (`migrate_legacy`, idempotent); the file picks up the new form on its next save. Mixed
  strings are not migrated.

**Use points** (each covered by a test): MCP http/rest headers, url and body templates; proc
args and env; tunnel passwords, key passphrases and rule fields; job commands and env; the
panel's MCP Test endpoint; MCP matching (a failed resolve counts as no match).

**Masking.** Config echoed to the panel replaces literal secrets with the sentinel `••••••••`
and restores them on PUT (`unmask_body`). References are not secrets and pass through as-is,
including a scheme and one whole reference in an `env` or `headers` value (§mcp.defs).
The vocabularies (URL-embedded passwords, header names, parameter keys) are in `mask.rs`.
The echo also drops the dead `readonly` key an old MCP definition may still store (stored state
keeps it until the next save).
Output captured from a run masks exactly the values its refs resolved to.

## §host.vault — The secret vault

A namespace-isolated store for credential values, referenced as `${secret://name}` (ADR-014,
ADR-019). It is **host mechanism, not a plugin** — everything depends on it, so it has no
disabled state; its page is a host asset.

- **Storage.** `secrets.json`, sealed: `{rev, secrets: {name → value}, groups, secretGroups,
  order}`. Names match `[a-z][a-z0-9-]{0,63}`. Loaded once at boot into memory; it never joins
  the env lookup path.
- **Child isolation.** Only the env overlay is merged into child environments. The vault has no
  merge path at all — a key saved for one HTTP MCP is not readable by every proc MCP, job and
  shell. A test pins it.
- **API — values go in, never come out.** `GET /api/secrets` → names with update times, `rev`,
  `groups`, `secretGroups`, `order` (stored as-is, stale names included). `PUT
  /api/secrets/{name}` `{value, rev}` creates or overwrites; `DELETE /api/secrets/{name}`
  `{rev}` (also prunes the name from `order`). A missing rev is 400, a stale one 409, and the compare
  and the write are one step, so of several writers on one rev exactly one lands. A script
  stores a value with `swiss secret set` (§host.cli).
- **Order.** Rows are draggable within and across groups. Names in `order` sort by slot;
  unmentioned names follow in name order; an empty `order` is name order. Unknown names and
  duplicates are dropped on write; stale names are ignored on read. A value write never
  touches `order`.
- **Export.** Vault values leave `swiss export` as `******` and are typed in again after an
  import; import skips any value equal to the mask so it never overwrites a real secret.
- No zeroize (ADR-007): values must stay resident to be resolved; the attack surface is the
  file and the children, not the heap.
- **On the bus.** A put, a delete, a vault-group write and an import that stored a value
  publish HOST.VAULT (module 4, sub 5, an empty `Notice`) once the file is written and the
  in-process vault shows it. A refused write publishes nothing. An import whose entries are
  all skipped — masked, empty, or not a vault name — publishes nothing and leaves the file
  as it was. A change another process makes to the file is not announced; the next read
  shows it. The Secrets page re-reads `GET /api/secrets` from the notice and has no poll.
  LLM Agents re-reads `GET /api/llm/status` from the same notice (§llm.agents).
