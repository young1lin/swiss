# §security — The security model in one place

Chapter 0015 of the [swiss specification](../SPEC.md).

swiss holds the credentials a developer's machine runs on: SSH keys and passphrases, database
URLs, API keys, OAuth tokens and its own bearer tokens. The model is two walls — **the loopback
boundary** and **a device-bound sealed store** — plus a handful of rules that keep a value from
leaking between them. Each rule is specified where it is implemented; this section is the map,
and a change that weakens any line of it is a security change whatever else it does.

| Layer | Rule | Where |
|---|---|---|
| Bind | Only a loopback host is accepted; a non-loopback host is refused at config load | §host.boundary |
| Peer, `Host`, `Origin` | Every route checks all three; a refusal is `403 {error}` (DNS-rebinding defence) | §host.boundary |
| Framing | Every answer forbids framing (`frame-ancestors 'none'`, `X-Frame-Options: DENY`) | §host.boundary |
| Admin surface | Loopback **and** a credential: a single-use login token → a signed `HttpOnly; SameSite=Strict` cookie that counts only from the panel's own origin, or the rotating CLI key in `X-Swiss-Key` | §host.session |
| MCP clients | Named bearer tokens, compared timing-safe before the body is read | §host.tokens |
| Terminal stream | A 10 s single-use ticket bound to one session on top of the loopback guard, so loosening `Origin` can never loosen the terminal | §terminal.api |
| At rest | State files and module history bodies sealed (AES-256-GCM under a key derived from the device-bound master key); unix modes 0700/0600. What stays plain is listed below | §core.sealed, §core.masterkey, §core.home, §host.history |
| Credentials in config | References only — `${ENV}` or `${secret://name}`; one resolver at every use point | §host.refs |
| The vault | Values go in and never come out; not merged into any child environment | §host.vault |
| Models | A provider key is a reference resolved per client and scrubbed from every provider error; a Test asks the saved row, never a URL or key from a request; agent telemetry carries prompt text only with `logPrompts`, its headers resolved only into the export or a child's env | §llm.providers, §llm.api, §llm.telemetry |
| Echo | Literal secrets in echoed config are masked with a sentinel and restored on PUT | §host.refs |
| Run output | Every value a reference resolved to (≥ 8 bytes) is masked before it reaches a buffer, a record or a log | §process.actions, §remote.security |
| Logs and the call log | Params redacted and bounded; no credential ever written in the clear | §mcp.logs, §mcp.calls |
| Children | A config file is a code-execution boundary: nothing runs from viewing a page, saving config or an import preview; no shell unless the command names one. A managed app starts only on an explicit Start or a daemon boot | §process.actions, §process.apps |
| Kills | A hard kill re-identifies its target immediately before acting (port owner + image, or pid + creation time), never targets a stranger holding an app's port and never targets swiss | §process.apps |
| SSH | Host keys are trust-on-first-use with an explicit trust action; a mismatch refuses | §tunnels.ssh |
| Remote | A run has exactly the login user's rights; argv is quoted once by one function | §remote.security |

## §security.threats — What is in scope and what is not

**In scope** — anything that lets:

- a non-loopback peer reach the gateway or the panel (bind, `Host`/`Origin` checks, tunnels);
- a page on another local port, or any other page, use a signed-in browser's session on
  `/api/*` or frame the panel;
- a caller read a stored secret back out, or read another client's traffic or token;
- a `${secret://…}` reference, an env reference or a masked field leak its value through a
  log, an API answer, an error message or a run record;
- a proc/HTTP MCP definition, a job command or a remote target escape the process boundary
  the definition declares;
- a sealed state file be opened on a machine other than the one that sealed it.

**Out of scope:** an attacker who already runs code as the same user on the same machine (that
user owns the seal key by construction), and anything reachable only by deliberately widening
the bind past loopback — the config loader refuses it.

**Honest notes inside the boundary.**

- On Windows the seal key is DPAPI-bound to the user; on Linux the machine-id binds to the
  machine, not the user, so on a shared host another local user can derive it. macOS keeps it in
  the user's login Keychain (§core.masterkey).
- Not everything under the home is sealed, and for the rest its private placement is the boundary.
  `memory/store.sqlite3` and `memory/code.sqlite3` are plain SQLite (ADR-031, §memory.store). A
  module history seals its bodies and not its index, and the LLM record keeps a ~2 KiB plaintext
  preview of each call and turn (§llm.history). The gateway log, the job run lines and a managed
  app's capture file are plain text. A machine with no master-key source stores history bodies
  unsealed (§host.history).
- A bearer token **can** be read back in plaintext (`GET /api/tokens/{id}/secret`). Tokens and
  vault secrets are two credential classes with two threat models: a token is compared in
  plaintext anyway and sits in `managed.json`; the separate route makes the read an explicit
  act and saves a rotate before every copy of a connect command. Vault values never come back.
- No zeroize (ADR-007): resolved values must stay resident to be used; the attack surface is
  the file and the children, not the heap.
- Masking matches a resolved value's own bytes, at 8 bytes or more: a shorter value, or a program
  that re-encodes its own secret, gets past it, and output access and file modes remain the wall
  behind it. It does not reach the terminal, whose recording keeps the output as it came, an echoed
  secret included, in sealed parts (§terminal.recording).

## §security.reporting — Disclosure

`SECURITY.md` is the public policy: report privately (GitHub private vulnerability reporting,
or a private advisory draft), never in a public issue; acknowledgement within 7 days, a fix or
a decision within 30; only the latest release is supported, and CVE IDs go through GitHub
Security Advisories when a fix ships. `SECURITY.md` and this section must agree — change both
in one commit.

## §security.rules — Rules for every change

- A new route inherits the loopback guard and the session check by being mounted in the
  guarded trees; an unguarded tree merged into the app is a boundary hole, and route
  registration refuses clashes and reserved prefixes (§host.routes).
- A new credential field is a reference, resolved through `swiss_core::secure::refs` at its use
  point, masked when echoed, and covered by a test at that use point (§host.refs).
- A new state file is sealed, private-moded and, when it is per-instance, keyed by port
  (§core.home).
- A new child process goes through the supervisor or the same platform calls — job object or
  process group, the PID ledger, bounded capture, masking (§process.supervisor). A program
  meant to outlive the gateway goes through `spawn_detached` and nothing else
  (§process.detached).
- Error messages name the reference, the field and the use point — never the value.
- Tests and fixtures use neutral names only (§release.hygiene); a test key is a documented
  fixed test key, never a real one.
