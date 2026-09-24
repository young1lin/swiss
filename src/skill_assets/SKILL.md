---
name: swiss
description: Install and inspect the local swiss gateway only when the user explicitly asks; use swiss-remote for configured remote targets.
disable-model-invocation: true
---

# swiss setup and local checks

swiss is a single local executable: one gateway on `127.0.0.1:19999` with MCP servers,
database browsing, SSH tunnels, jobs, a terminal, and an admin panel. This skill covers
installation, updates, and non-secret local checks. Use the separate `swiss-remote` skill
for remote commands, file transfer, and run history.

## Boundaries

- Act only after the user explicitly requests installation, an update, or a local check.
  Do not install or start swiss just because this skill was loaded or a project mentions it.
- Bind and connect on loopback only. Never change the host to `0.0.0.0`, expose the panel
  through a public proxy, or create an SSH tunnel without a separate, explicit request.
- The panel and `/api/*` have no password; loopback is their network boundary. `/mcp/*`
  requires a bearer token. Another local process can reach the panel. Do not claim that
  the admin API is authenticated or that a local installation is safe on a shared host.
- Never print `swiss token` or `swiss creds` into an agent transcript, command log, or
  issue report. Do not copy `~/.swiss` or publish exported state; it holds sealed secrets.

## Install or update when asked

1. Identify the OS and architecture. The canonical packages and `SHA256SUMS` are at
   https://github.com/young1lin/swiss/releases. Stop if that release is unavailable;
   do not substitute an untrusted mirror or run a remote install script.
2. Tell the user which version, archive, and user-owned installation directory you will
   use. Obtain approval before replacing an executable, changing PATH, or starting a
   service. A running installation may need the user's own stop/update procedure.
3. Download the matching archive and `SHA256SUMS`. Compare the archive's SHA-256 with
   its listed value before extracting or executing it. On Windows, run
   `Get-FileHash <archive> -Algorithm SHA256`; on Linux use `sha256sum <archive>`,
   or on macOS `shasum -a 256 <archive>`. A mismatch is a hard stop.
4. Extract `swiss.exe` or `swiss` to the approved, stable location on PATH, not an
   ephemeral npx or agent cache. Keep the accompanying license and notice files.
5. Check `swiss --version` and `swiss status`. Run `swiss skill install` to install
   the embedded `swiss` and `swiss-remote` skills for supported local AI clients.
   Re-run it after an update so the skills match the binary.

macOS packages are experimental: the default master-key source is not implemented, so a
fresh instance cannot persist first-run state. Do not promise a working macOS setup or
quietly invent a persistent `SWISS_MASTER_KEY` for the user. Linux GNU packages may
require a compatible system glibc; a single-file package is not a fully static build.

## Inspect without changing state

```text
swiss --version      # verify the installed binary
swiss status         # check whether the gateway is running
swiss --help         # inspect commands supported by this version
```

A `swiss status` exit code of 3 means no gateway is running, not a failed installation.
Report it and ask before running `swiss start`; do not stop, restart, enable autostart,
import/export state, or open a panel as a side effect of inspection. Point the user to
`swiss open` only if they explicitly want the panel. For remote operations use
`swiss-remote` only after a separate explicit request.
