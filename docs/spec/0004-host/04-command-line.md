# The command line

Part 04 of [§host](README.md), chapter 0004 of the [swiss specification](../../SPEC.md).

## §host.cli — The command line

`swiss start | stop | restart | status | logs | token | creds | open | gui | export | import
<file> | skill list [--json] | skill install [<name>... | --all | --installed] | autostart [on|off] |
path [on|off] | update | serve | remote … | run … | process … | api <METHOD> <path> [json | --body-file <path|->] | secret set <name> [--from <source>] | secret ask <name>... [--wait <seconds>] | connect <claude|codex|cursor> [--mcp <name>]... [--replace] | setup …`. Options: `-p/--port` (start: listen and save as default; others: which instance),
`-f` (start: foreground; logs: follow), `--no-open`, `-n/--lines` (default 200), `--force`,
`--json`, `-h`, `-v`. `swiss --help` is the reference text; `remote` and `run` are §remote,
`process` is §process.cli.

- **`api`** — one admin call signed with the CLI key; what scripts use instead of curl.
  `--body-file` takes the body from a file, or stdin for `-`, rather than argv, where a value lands
  in the process list and the transcript and Windows PowerShell 5.1 strips its quotes. A file or
  stdin may carry a BOM or be UTF-16, which is what that shell writes.
- **`secret set`** — stores one vault secret (§host.vault) with its value off the command line. The
  value comes from `--from <source>` (`env:VAR`, `file:PATH`, `dotenv:PATH#KEY`, a relative path
  resolving against the working directory and a leading `~/` against the home directory) or piped
  stdin, which decodes as above. One trailing newline is trimmed, and an empty value is refused.
  There is no value flag, and a terminal stdin is refused rather than echoing. `dotenv:` reads
  `KEY=VALUE`, `export KEY=VALUE`, quotes and `#` comments with no interpolation, and the whole file
  must parse. A refusal quotes neither a word in the wrong place nor a source it does not know, either
  of which may be the value, and a parse error gives a line number and never the line. The gateway
  starts without a browser when it is down, a stale `rev` is read again up to three times, and an
  existing name is overwritten. It prints one JSON document on stdout, which never holds the value:
  `{ok, gateway: {port, started}, items: [{kind, name, status, refreshed, failed}]}`, or `{ok: false,
  error}` when refused. Exit 0: stored and every MCP using it rebuilt; 1: not stored, or an MCP did
  not rebuild; 2: refused, nothing written; 3: the gateway could not start.
- **`secret ask`** — opens the panel's Secrets page on the names a human still has to type
  (`#secrets?ask=a,b`), by the same one-time sign-in `swiss open` uses. The link is not printed.
  It returns at once with exit 4. `--wait <seconds>` polls `GET /api/secrets` until every name
  exists, and then the exit is 0. A name already stored is left out of the page. With no desktop
  to open, it says so and names the two other ways: forward the port over SSH and open the panel,
  or pipe the value into `swiss secret set` from the human's own shell. The page shows one row
  per missing name, the name fixed and the value focused, and it does not store anything by itself.
- **`connect`** — points Claude, Codex or Cursor at this gateway's MCPs. Claude is configured by
  `claude mcp add --transport http -s user`, so `$CLAUDE_CONFIG_DIR` is honoured. Codex is a
  `[mcp_servers.<name>]` block in `$CODEX_HOME/config.toml` (`type`, `url`, and
  `http_headers.Authorization`), the block the panel copies. Cursor is a merge into `~/.cursor/mcp.json`.
  The bearer is a token labelled `client-<client>`, reused when that label exists and minted
  otherwise, and it is never printed. `--mcp` repeats; without it, every MCP the gateway has.
  An existing entry is left unchanged unless `--replace`. When an entry was written, `next` says
  to restart the client; a plan, an unchanged entry and a failed one owe no restart.
- **Skill inspection.** `GET /api/skills`, behind the admin session, checks only the three
  embedded skill names in the two directories a skill goes to (below). It reads each `SKILL.md`,
  creates nothing, and returns `{skills: [{name, description, loadsByItself, locations: [{root,
  path, installed, state, error?}]}]}`. `installed` is true/false, or null when the filesystem
  refused the check; `state` is `absent`, `current`, `outdated` or `unreadable`. There is no idle
  scanner.
- **Selected skill installation.** `POST /api/skills/install` accepts `{name, root}`, restricted
  to the three embedded names and `.claude` / `.agents`. It stages and atomically replaces that
  target's `SKILL.md` and, for the `swiss` skill, `SETUP.md` beside it, preserving other files and
  skills, then returns the current inspection shape. A `swiss` copy whose `SETUP.md` is missing or
  differs is `outdated`. Unknown targets and extra request fields are refused before filesystem
  access. Writes run
  on the blocking pool and require the same admin session.
- **`export` / `import`** — the one plaintext escape: a v1 bundle of config, managed, tunnels,
  OAuth, env and secrets (vault values masked, §host.vault). It moves those six and no more:
  `remote.json`, `memory/store.sqlite3`, the LLM records and each module's history databases stay
  behind. A file that cannot be read stops the export (§core.sealed): exit 1, the file named on
  stderr, nothing on stdout. Import refuses a section that is not an object and a state file it
  cannot read before anything changes, then lands every file re-sealed on this machine as one
  change, creating the home on a new machine; env and secrets merge (imported values win, nothing
  deleted). The CLI warns that the rest of the bundle is plaintext.
- **`autostart`** — start at sign-in: a registry Run value on Windows, a LaunchAgent on macOS,
  a systemd user unit on Linux. All three run `start` once and supervise nothing, so a gateway that
  crashes stays down until someone starts it. The unit is `Type=oneshot` with `RemainAfterExit=yes`.
  The default type would have the manager kill the gateway when `start` exits, and `Type=forking`
  has it guess the main process, which fails once an app that outlived an earlier gateway
  (§process.detached) is in the cgroup. `ExecStop` runs `stop` and ignores its answer, because the
  gateway may already be gone, and `KillMode=process` leaves those apps running. So
  `systemctl --user status swiss` says the start ran, and `swiss status` says whether a gateway is
  up. The OS registration is the store (no revision to race); `GET/PUT /api/autostart` read and
  apply it.
- **`path`** — swiss on the current user's PATH. Windows: this exe's folder as an entry of the
  user's own `Path` (HKCU\Environment, its REG_EXPAND_SZ type kept), then a
  `WM_SETTINGCHANGE` broadcast as `setx` sends. An entry counts as on however it is written
  (`%VAR%` expanded, quotes, a trailing `\`, case), and off removes exactly those entries. A
  `Path` that exists but is not a string is an error, never overwritten. Unix: a
  `~/.local/bin/swiss` symlink to this exe; only a symlink resolving here is on or removed, and
  a real file in the way is refused. Running programs keep their PATH. `GET/PUT
  /api/user-path` read and apply it.
- Both machine settings reach the admin API through `AppContext` seams (`autostart`,
  `user_path`) that only the boot sequence sets to the OS providers, so no test composition can
  write the host (§testing.rules).
- **`update`** — checks GitHub for a newer release; updating stays a manual exe swap, then
  `swiss skill install --installed`.
- **`skill list` / `skill install`** — the embedded `swiss`, `swiss-remote` and `swiss-memory`
  skills, and which of them land is the user's answer:

  ```text
  swiss skill list [--json]        each skill: what it does, whether it loads by itself, where it is,
                                   and whether that copy still matches this binary
  swiss skill install              at a terminal, asks per skill, default no;
                                   with no terminal, refuses with exit 2 and the list
  swiss skill install <name>...    exactly these
  swiss skill install --all        every shipped skill, said out loud
  swiss skill install --installed  refresh what is already there; never adds. What an update runs
  ```

  `--json` prints the inspection shape above. An unknown name or two selectors at once refuse with
  exit 2 before anything is written; exit 1 is a write that failed. A skill goes to exactly two
  directories: `.claude` for Claude Code (`$CLAUDE_CONFIG_DIR/skills` when that is set, because
  Claude Code then reads nothing under `~/.claude`) and `.agents` for every other client
  (`skill_install::tests::skills_go_to_exactly_the_two_directories`). The CLI, `GET /api/skills` and
  `POST /api/skills/install` share that one list. Copies an older build left in `~/.cursor/skills`
  or `~/.codex/skills` are not touched and not refreshed
  (`a_refresh_leaves_a_copy_outside_the_two_directories`). Whether a skill loads by itself is read
  from its `disable-model-invocation` line, not written twice.

  Skills ship inside the binary, so a new version may change them, and a copy on disk does not
  change by itself. `skill list` marks a copy that differs from this binary's as `outdated`, which
  includes a `SKILL.md` edited by hand, or a `swiss` copy whose `SETUP.md` differs. Updating swiss
  is therefore two steps: replace the executable, then `swiss skill install --installed`;
  `SETUP.md`, which the `swiss` skill points at, and what `swiss update` prints both say so. No gateway boot and no panel load refreshes a skill behind the user's back
  (`cli::tests::no_terminal_and_no_names_refuses_and_installs_nothing`,
  `a_terminal_prompt_defaults_to_no`, `skill_install::tests::installed_refreshes_and_never_adds`).

  Every write stages `SKILL.md` beside the target and renames it over, and the `swiss` skill's
  `SETUP.md` the same way, so what the user keeps in that directory stays and a replacement that
  fails leaves the old copy
  (`a_reinstall_keeps_what_the_user_put_beside_the_skill`,
  `a_failed_replacement_keeps_the_old_copy`). A denied rename is tried again for about a second:
  Windows refuses one while another process holds the file open, and an AI client watching its
  skills directory opens a fresh `SKILL.md` for a few milliseconds
  (`a_staged_skill_held_open_for_a_moment_still_lands`,
  `a_denied_step_is_tried_again_and_another_error_is_not`).
- **`creds`** prints the panel URL and the gateway token; **`open`** mints a login link; **`gui`**
  spends one in a window of its own (§host.gui).
- **`setup`** configures a machine from a manifest (§host.setup).

## §host.setup — Configuring from the command line

An Agent or a script configures a machine by describing it. `src/setup_cli/` reads a JSON manifest
and drives the same `/api/*` routes the panel uses, signed with the CLI key (§host.session). It
holds no state and adds nothing to the daemon.

```text
swiss setup schema [section | plugins.<id>]   what a manifest may say
swiss setup show                         this gateway's state as a manifest, without values
swiss setup guide                        print SETUP.md; no gateway
swiss setup plan <file|-> [--rotate-secrets]   what it would change here; writes nothing
swiss setup apply <file|-> [--no-check] [--rotate-secrets]   make it so, then check what it made
```

`-` reads stdin. A file or stdin may carry a BOM or be UTF-16, which is what Windows PowerShell 5
writes, and a terminal stdin is refused. Each prints one JSON document on stdout, progress goes to
stderr, and none prompts. `setup plan` and `setup apply` start the gateway without a browser if it is
down, and say that they did, but only once the manifest has been read and found sound. `setup show`
starts it the same way, having nothing to read first. A gateway answers before its plugins have
started (§host.plugins): an entry reads `disabled` until the boot reaches it, and `idle` is not a
start. So plan, apply and show wait for the boot to finish, up to 60 s
(`a_gateway_mid_boot_is_waited_for`). `setup schema` needs a gateway only for `plugins.<id>`, what that
plugin's row takes, which only the plugin knows; the row itself stays out of the answer.
`setup guide` prints the embedded `SETUP.md` and does not start a gateway. Both skill install
paths write that file beside the `swiss` skill.

`setup show` prints that state as a version-1 manifest and writes nothing. A secret's name is `ask`,
because the vault cannot be read back and the source is not stored. A slot that held a credential
holds the mask sentinel; a `${secret://name}` or `${ENV_VAR}` reference is kept, since it is not the
value, and a reference's default is masked with the rest of the slot because the default is typed
text. `machine.path` and `machine.autostart` are the booleans those seats report, and stay out when
this process cannot read them. `skills` names the shipped skills present in the caller's directories.
A plugin that is not serving is left out of the sections it holds, and so is a connection or rule
name that two rows share. Absence means untouched, and show does not guess or delete. Giving that
manifest to `plan` or `apply` is refused wherever the mask sits
(`show_prints_a_manifest_whose_mask_plan_refuses`).

**The manifest.**

```json
{
  "version": 1,
  "secrets": {
    "github-token": "env:GITHUB_TOKEN",
    "glm-key": "dotenv:.env#ZAI_API_KEY",
    "deploy-key-pass": "file:~/.config/acme/pass",
    "prod-db-password": "ask"
  },
  "plugins": {
    "llm": { "config": { "providers": { "glm": {
      "style": "anthropic", "baseUrl": "https://open.bigmodel.cn/api/anthropic",
      "apiKey": "${secret://glm-key}"
    } } } },
    "jobs": { "config": { "definitions": { "backup": {
      "trigger": { "kind": "cron", "expression": "0 3 * * *" },
      "action": { "type": "process.exec", "input": { "program": "restic", "args": ["backup", "."] } }
    } } } }
  },
  "mcps": {
    "github": {
      "type": "http", "url": "https://api.githubcopilot.com/mcp/",
      "headers": { "Authorization": "Bearer ${secret://github-token}" }
    },
    "playwright": { "command": "npx", "args": ["-y", "@playwright/mcp@latest"] },
    "prod-db": {
      "type": "mysql", "host": "127.0.0.1", "port": 13306, "user": "reader",
      "password": "${secret://prod-db-password}", "database": "app"
    }
  },
  "clients": {
    "claude": {},
    "codex": { "mcps": ["github"] },
    "cursor": { "mcps": "*" }
  }
}
```

Every section is optional. A client's own `.mcp.json` is a valid manifest: `mcpServers` and
`servers` are accepted for `mcps`, and entries normalise as §mcp.import does (`command` + `args`,
a `url` becomes `http`, an entry pointing at this gateway is skipped with the warning `mcp.skipped`,
and a key that is the client's own is dropped with the warning `mcp.keys-dropped`). Unlike an import,
a name stays exactly as written, and one that is not a valid MCP name is refused rather than rewritten.

| Section | Identity | Meaning |
|---|---|---|
| `secrets` | name | Source of the value. `env:VAR`, `file:PATH`, `dotenv:PATH#KEY` or `ask`. Relative paths resolve against the manifest's folder, or the working directory for stdin, and a leading `~/` against the home directory |
| `plugins` | the plugin's id | `{enabled?, config?}`. `config` is a merge patch over the plugin's row (§host.config): objects merge, arrays and everything else replace, and what it does not name stays, so `plugins.jobs.config.definitions.backup` adds one job. Validated by the plugin's own validator, under `plan` too |
| `mcps` | name | The whole MCP definition (§mcp.defs), keys per type |
| `machine` | setting or skill name | Optional `path` and `autostart` booleans use the panel's routes. `skills` is an explicit list of shipped names, installed in the caller's skill directories as `swiss skill install` does; `true` is refused, and no skill is removed |
| `tunnels.connections` | name | Whole connection definition (§tunnels.store), with `jump` naming another connection. `port` defaults to 22, `authType` to `key`; key auth needs `keyPath`, password auth needs `password`. A stored group and host key stay when omitted |
| `tunnels.rules` | name | `connection` names a connection, `localPort` and `targetPort` are required. `targetHost` defaults to loopback, `autoReconnect` to false, `reconnectInterval` to 10 seconds. Omitted `mcps` and group stay. `start: true` asks for a start after checks; otherwise no new rule is started |
| `remote.targets` | id | `endpoint` names a connection and `root` is an absolute POSIX workspace path. Optional `caps` uses §remote.targets' capabilities and includes `exec`; a new target defaults to `exec`, an existing target keeps omitted capabilities, label, group and timeout |
| `clients` | claude, codex or cursor | `{mcps?}`. Left out, the MCPs this manifest names. `"*"` is every MCP the gateway has. An existing client entry is left unchanged; `swiss connect --replace` rewrites it |

Names in tunnel and remote references resolve against this manifest or the gateway. Two stored
connections or rules sharing a name fail that item with their ids; no match is guessed. Jump hops
are written before the connections using them, and cycles, conflicting local ports, unknown fields
and unsupported reference locations are refused before any write. Tunnel credentials are whole-field
references, never literals. `setup schema machine`, `tunnels` and `remote` describe these sections.

**Host-key confirmation.** `hostKey`, when supplied, is an OpenSSH SHA256 fingerprint. A successful
connection check with no pin returns `needs-input`, code `connection.host-key`, `fingerprint` and
`saved: true` when newly saved. The next step asks a human to compare it with a trusted source and
put `hostKey` in the manifest. The test pins nothing. A requested rule start is held if the connection
or any jump has no pin, even under `--no-check`. Saving an edit can restart an existing running rule
inside the manager: if that restart would use an unconfirmed key, the item fails as
`connection.host-key-required` or `rule.host-key-required` before the write. Pin it first or stop the
affected rule. A mismatched key fails the check as `check.host-key`, reporting the presented fingerprint.

**Refused before anything is written** (exit 2, every problem listed, with its path, and no gateway
asked or started):

- an unknown field, with the nearest known one (`mcps.db.pasword: unknown field; did you mean
  password?`);
- a credential literal in any credential-named field, header, env key, URL password or command
  argument, recognised by the `mask.rs` vocabularies, `literal::find` and
  `swiss_memory::secrets::scan`. The message never quotes the value, and says what to write: a
  `secrets` entry and a `${secret://name}`. A reference's default is typed text, so it is refused
  the same way, and so is a malformed reference. There is no override. A credential-named slot that
  holds something else has a way out: the value goes in the vault, and the slot holds the reference;
- the mask sentinel, which only a masked read can have produced;
- `null`, a `version` other than 1, a bad secret or MCP name, one name in two MCP blocks, a source
  syntax this build does not know, or `plugins.mcp.enabled: false` in a manifest that names MCPs.

A `${ENV_VAR}` reference is accepted with the warning `ref.env-unverifiable`: the CLI cannot see the
daemon's environment, and a missing variable expands to empty (§host.refs). `${secret://name}` is the
form to write.

**What a plan reads.** GETs, each secret's source, which is read here in the caller's process and
dropped and never reaches the output, and each changed plugin row put to its validator with
`?dryRun=1`, which persists nothing. A gateway on another build is not asked, because an older one would
save what it was only asked about: that plugin carries the warning `plugin.not-validated`, and `apply`
is what validates it. A plan starts no MCP and writes nothing
(`plan_writes_nothing_and_starts_nothing`). An existing secret stays `unchanged`, because the vault
cannot be read back and a write rebuilds every MCP using it. `--rotate-secrets` says what a rewrite
from each readable source would do, and a source it cannot read keeps the stored value with the
warning `secret.not-rotated`. A secret a definition or a plugin patch references that neither the
manifest nor the vault provides is an item of its own: `needs-input`, code `secret.missing`. An MCP or a
plugin using a secret that is `failed` or `needs-input` is `blocked`, naming it in `by`.

An MCP is compared by building the manifest's definition with `build_def`, masking it as the gateway
masks a read-back, and comparing the two
(`a_definition_already_on_the_gateway_reads_as_unchanged`). A definition the mask reaches into, such
as a command holding `-p8080:80`, cannot show a change there, so `unchanged` carries the warning
`mcp.compare-masked`. A gateway running a build other than the CLI's adds `gateway.stale-build`.

A plugin is compared by merging its patch over the row and comparing the result, and by whether it is
as `enabled` asks (`a_plugin_patch_keeps_the_rest_of_the_row`). Asked on and `failed` reads as a change,
so a rerun retries the start. What its validator refuses is `failed`, code `plugin.invalid`, and an id
the gateway does not have is `failed`, code `plugin.unknown`, naming the nearest and never put in a URL;
neither stops the rest. A warning its validator returns is the warning `plugin.warning`. A manifest holds
no `null`, so a patch cannot remove a key: one an older build left, which the plugin no longer reads, fails
the merged validation as `plugin.invalid`, and removing it is `swiss api`.

MCPs live in the `mcp` plugin. If someone turned it off, every named MCP is `failed`, code
`mcp.plugin-off`, saying how to turn it on; a manifest naming none is not affected. A manifest that says
`plugins.mcp.enabled: true` carries the warning `mcp.plugin-off` under `plan`, because what the plugin
holds cannot be read yet; `apply` turns it on and then decides each MCP against what it finds
(`a_manifest_turns_on_the_plugin_its_mcps_need`).

**What apply promises.**

- Additive: what the manifest does not name is untouched. Removal is `swiss api DELETE`.
- Idempotent: the same manifest applied again reports only `unchanged`. Under `--no-check` that run
  sends no non-GET at all; with checks it sends only the checks
  (`apply_twice_is_all_unchanged_and_writes_nothing`).
- A stored secret is not rewritten, because the vault cannot be read back and a write rebuilds every
  MCP using it. `--rotate-secrets` rewrites those with a readable source.
- A race retries: a 409 on a vault `rev` or a plugin `revision` re-reads, re-merges and retries, three
  times, so what the other writer saved stays (`a_conflict_rereads_and_retries`). Every plugin write
  carries its revision.

**The order.** Secrets, plugins (config, then enable, so a plugin turned on starts on what the manifest
says), machine settings and chosen skills, connections (jump hops first), rules saved without an
explicit start, remote targets, then MCPs, each added with `start: false`, then every check, then clients.
A failed item does not stop the rest; what depends on it is `blocked`. A plugin write the gateway refuses is code
`plugin.write`.

A machine setting is judged by the state its route reads back. A write the seat refused, or an answer
that still holds the old state, is `failed`, code `machine.write`, and a rerun tries it again
(`a_seat_write_that_did_not_land_fails_and_is_tried_again`). A seat that cannot be read is code
`machine.unavailable`. A skill copy that cannot be read is code `skill.read` and is not overwritten, and
one that cannot be written is code `skill.write`.

Disabled `tunnels` or `remote` plugins fail their items with `tunnels.plugin-off` or
`remote.plugin-off`; a manifest enabling them carries a plan warning and decides the items again
after enabling. Disabling a plugin while naming items that need it is refused. A connection's failed
secret or jump blocks its dependent connections, rules and targets. Every named connection is tested
through `/api/tunnels/connections/{id}/test` unless `--no-check`; targets are saved without executing
remote commands. Rule starts run only after connection checks and the host-key guard.

**The checks.** At most four run at once, each bounded at 75 s, and an unchanged MCP is checked too:
exit 0 says every check passed, and a rerun is how an Agent learns its fix landed.

| MCP | Check |
|---|---|
| `http`, `rest`, `mysql`, `mariadb`, `pg`, `redis`, `mongo` | `POST /api/mcpdefs/test`, 5 s. It is started as well unless it is lazy |
| `proc` | Started, tools listed, then left to the idle reaper |
| Any other type | Started, tools listed |
| OAuth | Not started or checked; `needs-input`, code `mcp.oauth`, until authorized |

| Plugin | Check |
|---|---|
| Any | Enabled and `failed` is a failed check, code `plugin.start`, under `--no-check` too. A row the running plugin did not take is code `plugin.apply` |
| `llm` | `POST /api/llm/providers/models` for each provider the patch names, unchanged or not: a listing. Never `providers/test`, which is billed. An endpoint with no listing fails here, code `check.provider`, and `--no-check` is the way past it |

A failure reads `check: {ok: false, code, error}`, with code `check.start`, `check.tools`,
`check.unreachable`, `check.timeout`, `check.provider`, `plugin.start` or `plugin.apply`. A provider's
failure names the provider and not its address (`a_provider_is_checked_by_its_listing`). `--no-check`
skips the MCP and provider checks and leaves a `proc` to its first request; every other MCP still starts,
and a start that fails is still reported. An MCP someone stopped stays stopped, with the warning
`mcp.left-stopped`, and a typed one is still tested.

**The output.** One document, on success and on failure alike. It never holds a credential, a
token, a one-time link or a secret's value.

```json
{
  "ok": false,
  "mode": "apply",
  "gateway": { "port": 19999, "started": true, "build": "6a01f5eb" },
  "summary": { "created": 2, "updated": 0, "unchanged": 1, "failed": 0, "blocked": 1, "needsInput": 1 },
  "items": [
    { "kind": "secret", "name": "github-token", "status": "created" },
    { "kind": "secret", "name": "prod-db-password", "status": "needs-input", "code": "secret.ask" },
    { "kind": "mcp", "name": "prod-db", "status": "blocked", "by": ["secret:prod-db-password"] },
    { "kind": "mcp", "name": "playwright", "status": "created",
      "executes": "npx -y @playwright/mcp@latest",
      "check": { "ok": true, "detail": "21 tools" } },
    { "kind": "mcp", "name": "github", "status": "unchanged",
      "check": { "ok": true, "detail": "answered in 212 ms" } }
  ],
  "warnings": [],
  "next": [
    { "who": "human", "do": "swiss secret ask prod-db-password",
      "why": "no source for this secret" },
    { "who": "agent", "do": "swiss setup apply swiss.setup.json", "why": "once that is done" }
  ]
}
```

`status` is one of `created`, `updated`, `unchanged`, `failed`, `blocked`, `needs-input`; under `plan`
the first two read `will-create` and `will-update`. A saved item whose check fails keeps its status and
sets `ok` false. `executes` names what a `proc` MCP will run, so whoever reviews a manifest can read it, under `plan`
too; a job patched into `plugins.jobs` is listed the same way, as a map by name, read from the patch, and
so are a `process` app's `start` and `stop` and an `llm` agent's `command`. A plugin's row never reaches
the document. A refused manifest prints `{ok: false, mode, problems: [{path, message}]}`, and bad usage or an unavailable
gateway prints `{ok: false, error}`, with `mode` for the latter.

| Exit | Meaning |
|---|---|
| 0 | Converged; every check passed |
| 1 | At least one item or check failed; rerun after fixing |
| 2 | Bad manifest or bad usage; nothing was written |
| 3 | The gateway is unavailable and could not be started |
| 4 | Nothing failed, but items wait for a human |

**A manifest is code.** A `proc` entry names a program, a job or a plugin row can name another, and
`apply` can write and run them. `executes` is what makes that reviewable, and `plan` never runs any of it.
