# swiss

<p><img src="assets/logo-wordmark.svg" alt="swiss" align="top" height="56"></p>

A developer's pocket multitool — one tiny local process, every tool behind one loopback port.

swiss serves on `127.0.0.1:19999`: every MCP server an AI client needs, on HTTP paths under
`/mcp/<name>`, plus database browsing with an agent beside the console, SSH tunnels, scheduled
jobs, remote execution, a web terminal, supervised processes, long-term project memory, and the
LLM providers and agents behind them — one binary with its admin panel embedded, no script
runtime, no `node_modules`, no npx wrapper.

**Why it exists:** memory. The Node.js gateway it replaced measured 113.8 MB RSS on a typical
workload; the same workload here reads **22.4 MB** (private bytes 8.6 MB). The numbers are
records, not gates — [SPEC §product.memory](docs/spec/0001-product.md#productmemory--the-memory-record) holds them.

![The swiss panel's Data page browsing a demo MySQL table](docs/assets/data-page.png)

## Install

Download the archive for your OS and `SHA256SUMS` from
[GitHub Releases](https://github.com/young1lin/swiss/releases):

| Asset | OS |
| --- | --- |
| `swiss-<version>-x86_64-pc-windows-msvc.zip` | Windows x64 |
| `swiss-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux x64 |
| `swiss-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux arm64 |
| `swiss-<version>-aarch64-apple-darwin.tar.gz` | macOS (Apple Silicon) — experimental¹ |
| `swiss-<version>-x86_64-apple-darwin.tar.gz` | macOS (Intel) — experimental¹ |

The Linux binaries need glibc 2.35 or newer (Ubuntu 22.04, Debian 12, or later).

This page describes the source, and the newest release can trail it:
[CHANGELOG.md](CHANGELOG.md) lists what is still unreleased.

Compare the archive's SHA-256 with its line in `SHA256SUMS`
(`Get-FileHash <archive> -Algorithm SHA256` on Windows, `sha256sum <archive>`
on Linux, or `shasum -a 256 <archive>` on macOS). Extract it, then put
`swiss.exe` or `swiss` on your PATH (`swiss path on` does it for your user). Each archive also includes `LICENSE`, `NOTICE`,
`THIRD_PARTY_NOTICES.md`, and standalone `skills/swiss/SKILL.md`,
`skills/swiss-remote/SKILL.md` and `skills/swiss-memory/SKILL.md` for inspection before
installing or running the binary.

¹ macOS builds store the master key in the user's login Keychain. Desktop bundles use the
  system WKWebView and require macOS 14 or newer. Local preview bundles use ad-hoc signing,
  not Apple notarization, so downloaded copies can require an explicit first-open approval.
  CI lints on the Apple Silicon runner
  for every change and builds both macOS targets for each release, but does not run the test
  suite there yet: a killed local shell's pty does not reach EOF on macOS,
  and the gateway's own memory reads 0 MB there.

```
swiss --version        # verify the installed binary
swiss skill list       # the AI skills this binary ships, and where each is installed
swiss skill install    # asks per skill whether to install it; the default is no
swiss status           # check whether a gateway is already running
swiss start            # start the gateway on 127.0.0.1:19999 and open the panel
swiss token            # the bearer token an MCP client authenticates with
swiss creds            # panel URL + token, ready to paste into a client
```

The opt-in [`swiss` skill](src/skill_assets/SKILL.md) covers safe installation, checksum
verification, and local checks. [`swiss-remote`](src/skill_assets/remote/SKILL.md) covers
configured remote targets, file transfer, and run history.
[`swiss-memory`](src/skill_assets/memory/SKILL.md) is the one skill for project memory: how
to find what it holds, ask it a question, and save what is worth keeping. All three are
embedded in the binary, and none lands until you choose it: `swiss skill install` asks per
skill, refusing and listing them when there is no terminal to ask at, and `swiss skill install
<name>...` installs exactly those (`--all` for every one). A skill goes to `~/.claude/skills` for
Claude Code (`$CLAUDE_CONFIG_DIR/skills` when that is set) and `~/.agents/skills` for every other
client. An install replaces `SKILL.md` and leaves anything else you keep in that directory.

A new version may change a skill, and a copy on disk does not change by itself: `swiss skill
list` marks one that differs from this binary as `outdated`, and `swiss skill install --installed`
refreshes the copies you have and adds none. No skill starts the gateway or exposes tokens just
because an agent loads it.

Or build from source: Rust stable, `cargo build --release` — the same single binary. Linux
additionally needs `cmake` and a C compiler (the TLS stack builds aws-lc-rs). Node is not
needed to build; see [CONTRIBUTING.md](CONTRIBUTING.md).

## Quick start

On Windows, double-click `swiss.exe`; on macOS, open `swiss.app` to use the desktop GUI.
On either desktop, running `swiss` without arguments or `swiss gui` opens the same window.
The explicit `swiss gui` command returns after launching it; use `swiss gui -f` to keep the
window in the foreground. Linux has no desktop window: the gateway serves and a browser looks.
The panel and AI skills are embedded in the single executable.
**Settings → System** shows skill installation in
`~/.claude/skills` and `~/.agents/skills`, alongside startup and PATH settings.

1. `swiss start` brings the gateway up on `127.0.0.1:19999` and opens the panel.
2. On the MCP page, **Add an MCP**: a stdio command (`npx …`, `uvx …`), a remote HTTP MCP,
   tools declared over a REST API, or a MySQL / PostgreSQL / Redis / MongoDB connection served
   by an in-process driver. The Data page browses all four — MongoDB as a documents workspace
   with a query bar, a document editor, an aggregation pipeline builder, schema analysis,
   indexes, explain plans and validation. **Import .mcp.json** brings an existing client config across in one
   step. A stdio MCP stays idle until its first request and is reaped again after idling, so
   one that is not in use costs nothing.
3. Connect a client from the MCP's `⋯` menu → **Connect a client**. *Copy Claude Code command*
   puts this on the clipboard with the live token filled in:

   ```
   claude mcp add --transport http -s user <name> http://127.0.0.1:19999/mcp/<name> --header "Authorization: Bearer <token>"
   ```

   The same menu copies a Codex `config.toml` block, a `.mcp.json` entry or the bare endpoint
   URL, and `swiss token` prints the token for any other client.
4. Keep credentials out of the config: a field takes `${ENV_VAR}` or `${secret://name}`, and
   **Settings → Secrets** stores a value once, write-only. References expand only at use time.

## Security model

- **Loopback only.** The gateway binds `127.0.0.1`, refuses every request whose `Host`/`Origin`
  is not loopback (DNS rebinding can make a remote page aim same-origin-looking requests at a
  loopback listener), and refuses a non-loopback `host` in config at load. Reach it remotely by
  forwarding the port over SSH, never by widening the bind.
- **A sign-in for the panel and `/api/*`.** `swiss start` and `swiss open` open a single-use
  link that expires in two minutes and trades itself for an `HttpOnly`, `SameSite=Strict`
  cookie that counts only from the panel's own pages (a page on another local port cannot ride
  it, nor frame the panel); the CLI signs its calls with a key rotated on every start
  (`swiss api` is the scripted way in). Another local user or a process that only knows the
  port gets 401.
- **A bearer token for `/mcp/*`**, checked before the body is read, so other tools on the
  machine cannot use your MCP servers unchallenged. The terminal WebSocket adds a single-use
  ticket that burns in 10 seconds.
- **Credentials stay out of run records, within limits.** Run output has every value a reference
  resolved to masked before it is stored, when that value is 8 bytes or longer. A shorter value,
  or a program that re-encodes its own secret, gets past it. A terminal recording is not masked:
  it keeps what the shell echoed, in sealed parts, so treat recordings as sensitive.
- **What the Data page's agent can write.** On MySQL and PostgreSQL a write it drafts lands in the
  console for you to run. Switch a connection to *Auto writes* and one narrow INSERT, REPLACE,
  UPDATE or DELETE runs by itself in a transaction, committed only if it changed at most 1,000
  rows. DDL, and an UPDATE or DELETE with no real WHERE, stay yours to run. On MongoDB and Redis
  its writes are always left for you.

If you forward the port over SSH, the far side gains the token boundary — not the loopback one.
The full model is [SPEC §security](docs/spec/0015-security.md).

## Update

`swiss update` compares the running build with the newest GitHub release and prints the steps.
The update itself stays a manual swap, on purpose — a downloader inside a resident process is
the opposite of the memory budget this project exists for.

```
swiss update     # "up to date", or the newest release and its download link
swiss stop
# replace swiss(.exe) with the binary extracted from the new release archive
swiss skill install --installed   # the new binary's skills, over the copies you already have
swiss start
```

Nothing migrates during an update: all state lives under the swiss home directory
(`%USERPROFILE%\.swiss` on Windows, `~/.swiss` elsewhere), never inside the binary.

## Your data, and moving machines

State files and history bodies in the home are sealed. On Windows the seal binds to your user
(DPAPI); on macOS the key sits in your login Keychain; on Linux it binds to the machine via the
world-readable machine-id, not to your user — treat shared hosts accordingly.

Not everything is sealed, and for the rest the home's private placement is the only boundary:

- `memory/store.sqlite3` and the code index beside it are plain SQLite.
- A module's history database seals its bodies but not its index, and the LLM record keeps about
  2 KiB of each call and turn in the clear.
- The gateway log, job run lines and managed apps' capture files are plain text.

So don't publish or hand over the home directory.

`swiss export > bundle.json` then `swiss import bundle.json` carries config, managed state,
tunnels, OAuth, env and secrets to another machine, and that is not everything. `remote.json`, the
memory store, the LLM records and each module's history stay behind. Vault values come out masked,
so set them again on the new machine. The rest of the bundle is plaintext, tokens and env values
included: keep it out of a repository and delete it once it has landed.

## Start at sign-in, and swiss on your PATH

The panel's Settings → Plugins page has a This machine section with two switches, "Start swiss
when you sign in" and "Put swiss on your PATH". Both act for your user only. From the terminal:

```
swiss autostart on     # Windows: an HKCU Run value · macOS: a LaunchAgent · Linux: a systemd user unit
swiss autostart off
swiss path on          # Windows: this exe's folder in your user Path · elsewhere: ~/.local/bin/swiss
swiss path off
```

A PATH change reaches terminals opened afterwards; ones already open keep their old PATH.

## Documentation

The design record is one living specification, [`docs/SPEC.md`](docs/SPEC.md): what swiss does
now, organised by area, one numbered chapter per area under [`docs/spec/`](docs/spec/) (product
and memory, architecture, formats, the host, each plugin, the panel, security, testing,
release), with the decision log (ADR-001 …) as its last chapter. `SPEC.md` is its front page:
how to read and amend it, the chapter table and a map from every anchor to its chapter.

## Non-goals

- **Never remote.** Loopback-only is the boundary; forward over SSH instead of widening it.
- **No self-updating binary.** `swiss update` checks and instructs; the swap is yours.
- **No saving on `proc` MCPs.** An `npx`/`uvx` child is 50–150 MB and stays exactly that.

## Contributing and security

[CONTRIBUTING.md](CONTRIBUTING.md) covers the build, the gates and the house rules (the long
version is [AGENTS.md](AGENTS.md)); [SECURITY.md](SECURITY.md) says how to report a
vulnerability privately — swiss holds credentials, so please use it rather than a public issue.

## License

Apache License 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE). Third-party material and its
terms are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md): the panel vendors xterm.js
and its addons (MIT), cronstrue (MIT) and shlex (MIT) under `crates/swiss-panel/src/admin_assets/js/vendor/`,
each directory carrying the upstream license text; the zai-vision adapter reproduces prompts
from `@z_ai/mcp-server` (Apache-2.0, Z.AI); the binary statically links the crates the file
tabulates.
