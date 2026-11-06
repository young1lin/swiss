# Installing and configuring swiss

swiss is one local program. It listens on `127.0.0.1` only, and the panel, the tools and the
MCP servers live in that one process. You configure it by running it. You do not edit its
source, and you do not print its secrets.

## How to tell what this machine has

```text
swiss --version
swiss status
```

`swiss status` exit code 3 means no gateway is running. That is not a failed install. Ask
before `swiss start`. Do not stop, restart, enable autostart, import or export state, or open
the panel as a side effect of looking.

## Install

1. Say which version, which archive and which directory you will use, and wait for approval
   before you replace a program, change PATH, or start anything.
2. Download that archive and `SHA256SUMS` from
   https://github.com/young1lin/swiss/releases. If the release is not there, stop. Do not use
   another site and do not run a remote install script.
3. Compare the archive with `SHA256SUMS` before you open it. On Windows,
   `Get-FileHash <archive> -Algorithm SHA256`. On Linux, `sha256sum <archive>`. On macOS,
   `shasum -a 256 <archive>`. A mismatch is a stop.
4. Put `swiss.exe` or `swiss` on PATH, in a directory the user owns. Keep the license files
   that came with it.

Windows archives are `.zip`. Linux and macOS archives are `.tar.gz`.

macOS packages are experimental: the default master-key source is not implemented, so a fresh
instance cannot persist first-run state. Do not promise a working macOS setup, and do not
invent a `SWISS_MASTER_KEY` for the user. Linux GNU packages need a glibc at least as new as
the one they were built with.

## Skills

Run `swiss skill list` and ask which skills to install. Installing none is valid. Never pick
for them. Then `swiss skill install <name>...`.

Skills are inside the binary. Replacing the program does not change the copies on disk. After
an update, run `swiss skill install --installed`. It refreshes what is already installed and
adds nothing.

## Update

Replacing the program is a manual copy of the new exe over the old one, after the checksum
matches. Then `swiss skill install --installed`.

## Configure

The loop is a JSON manifest and these commands. Each one prints one JSON document and never
prints a secret, a token, or a one-time link.

```text
swiss setup schema
swiss setup show
swiss setup plan <file>
swiss setup apply <file>
swiss setup guide
swiss secret set <name>
swiss secret ask <name>
swiss connect <claude|codex|cursor>
```

`swiss setup schema` is the field list. `swiss setup show` is this gateway's state with no
values. `swiss setup plan` writes nothing. `swiss setup apply` makes the manifest true.
`swiss setup guide` prints this file and does not start a gateway.

A secret whose value you do not have is `"ask"`. `swiss secret ask <name>` opens the panel on
that name. `swiss secret set <name>` stores a value from a pipe or from a file, never from the
command line. `swiss connect` points Claude, Codex or Cursor at this gateway. Tell the user to
restart that client afterwards.

Before `swiss setup apply` of a manifest you did not write, read `executes` in
`swiss setup plan`. That is the command a `proc` MCP will run.

## Rules that do not bend

- Never print `swiss token` or `swiss creds`.
- Never run `swiss export`.
- Never put a value in a manifest or on a command line.
- Bind to loopback only. Do not change the host to `0.0.0.0` and do not publish the panel.
- `swiss api` is how you call `/api/*`. Do not curl it.

## Exit codes

| Code | Meaning |
|---|---|
| exit code 0 | Converged |
| exit code 1 | Something failed; fix it and run the command again |
| exit code 2 | Bad usage or a bad manifest; nothing was written |
| exit code 3 | No gateway. `swiss status` means nothing is answering; a setup command also could not start one |
| exit code 4 | Nothing failed, and a person still has to do the `next` step |

`needs-input` is exit code 4. The document's `next` says who acts and the exact command. A
secret with no source asks for `swiss secret ask <name>`.
