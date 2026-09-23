# Security policy

swiss holds the credentials a developer's machine runs on: SSH keys and passphrases, database
URLs, API keys, OAuth tokens, the panel's own bearer tokens. Its security model is the loopback
boundary plus a device-bound sealed store; a hole in either is a serious bug and is treated as
one.

## Reporting a vulnerability

Please do **not** open a public issue for anything that could be a vulnerability.

- Preferred: GitHub's private vulnerability reporting on this repository
  ("Security" tab → "Report a vulnerability").
- Or email **young1lin** with `[swiss security]` in the subject.

Include what you can: the version or commit (`swiss --version` prints the build stamp), the
platform, steps to reproduce, and what an attacker gains. A minimal reproduction is worth more
than a long description.

You will get an acknowledgement within 7 days and a fix or a decision within 30. Credit goes to
the reporter in the release notes unless they prefer otherwise.

## Scope

In scope — anything that lets:

- a non-loopback peer reach the gateway or the panel (bind, `Host`/`Origin` checks, tunnels);
- a caller read a stored secret back out (the vault is write-only by design), or read another
  client's traffic or token;
- a `secret://` reference, an env reference or a masked field leak its value through a log,
  an API answer, an error message or a run record;
- a proc/HTTP MCP definition, a job command or a remote target escape the process boundary the
  definition declares;
- a sealed state file be opened on a machine other than the one that sealed it.

Out of scope: attacks that require the attacker to already run code as the same user on the
same machine (that user owns the sealed store's key by construction), and anything reachable
only by deliberately widening the bind past loopback — the documentation says not to, and
the config loader refuses it.

Two honest notes inside that boundary: on Windows the seal key is DPAPI-bound to your user,
while on Linux the machine-id binds to the machine, not the user — on a shared host a
different local user can derive the seal key, so treat multi-user machines accordingly. And
one deliberate exception to the secret read-out scope: the panel can read back a bearer
token it issued, in plaintext (GET /api/tokens/{id}/secret) — the token is stored
verbatim for constant-time comparison, the loopback caller is the same user, and the
separate route makes the read explicit. Vault secrets never come back out.

## Supported versions

Only the latest release receives fixes. There is no LTS line. CVE IDs are requested
through GitHub Security Advisories when a fix ships.
