# The command line

Part 04 of [§host](README.md), chapter 0004 of the [swiss specification](../../SPEC.md).

## §host.cli — The command line

`swiss start | stop | restart | status | logs | token | creds | open | gui | export | import
<file> | skill list [--json] | skill install [<name>... | --all | --installed] | autostart [on|off] |
path [on|off] | update | serve | remote … | run … | process … | api <METHOD> <path> [json | --body-file <path|->] | secret set <name> [--from <source>] | setup …`. Options: `-p/--port` (start: listen and save as default; others: which instance),
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
- **Skill inspection.** `GET /api/skills`, behind the admin session, checks only the three
  embedded skill names in the two directories a skill goes to (below). It reads each `SKILL.md`,
  creates nothing, and returns `{skills: [{name, description, loadsByItself, locations: [{root,
  path, installed, state, error?}]}]}`. `installed` is true/false, or null when the filesystem
  refused the check; `state` is `absent`, `current`, `outdated` or `unreadable`. There is no idle
  scanner.
- **Selected skill installation.** `POST /api/skills/install` accepts `{name, root}`, restricted
  to the three embedded names and `.claude` / `.agents`. It stages and atomically replaces only
  that target's `SKILL.md`, preserving other files and skills, then returns the current inspection
  shape. Unknown targets and extra request fields are refused before filesystem access. Writes run
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
  a systemd user unit on Linux. The OS registration is the store (no revision to race);
  `GET/PUT /api/autostart` read and apply it.
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
  includes a `SKILL.md` edited by hand. Updating swiss is therefore two steps: replace the
  executable, then `swiss skill install --installed`; the `swiss` skill and what `swiss update`
  prints both say so. No gateway boot and no panel load refreshes a skill behind the user's back
  (`cli::tests::no_terminal_and_no_names_refuses_and_installs_nothing`,
  `a_terminal_prompt_defaults_to_no`, `skill_install::tests::installed_refreshes_and_never_adds`).

  Every write stages `SKILL.md` beside the target and renames it over, so what the user keeps in
  that directory stays and a replacement that fails leaves the old copy
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
swiss setup schema [section]             what a manifest may say
swiss setup plan <file|-> [--rotate-secrets]   what it would change here; writes nothing
swiss setup apply <file|-> [--no-check] [--rotate-secrets]   make it so, then check what it made
```

`-` reads stdin. A file or stdin may carry a BOM or be UTF-16, which is what Windows PowerShell 5
writes, and a terminal stdin is refused. Each prints one JSON document on stdout, progress goes to
stderr, and none prompts. `setup plan` and `setup apply` start the gateway without a browser if it is
down, and say that they did, but only once the manifest has been read and found sound; `setup schema` needs no
gateway.

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
| `mcps` | name | The whole MCP definition (§mcp.defs), keys per type |

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
- `null`, a `version` other than 1, a bad secret or MCP name, one name in two MCP blocks, or a source
  syntax this build does not know.

A `${ENV_VAR}` reference is accepted with the warning `ref.env-unverifiable`: the CLI cannot see the
daemon's environment, and a missing variable expands to empty (§host.refs). `${secret://name}` is the
form to write.

**What a plan reads.** GETs, and each secret's source, which is read here in the caller's process and
dropped; it never reaches the output. It starts no MCP and writes nothing
(`plan_writes_nothing_and_starts_nothing`). An existing secret stays `unchanged`, because the vault
cannot be read back and a write rebuilds every MCP using it. `--rotate-secrets` says what a rewrite
from each readable source would do, and a source it cannot read keeps the stored value with the
warning `secret.not-rotated`. A secret a definition references that neither the manifest nor the
vault provides is an item of its own: `needs-input`, code `secret.missing`. An MCP using a secret
that is `failed` or `needs-input` is `blocked`, naming it in `by`.

An MCP is compared by building the manifest's definition with `build_def`, masking it as the gateway
masks a read-back, and comparing the two
(`a_definition_already_on_the_gateway_reads_as_unchanged`). A definition the mask reaches into, such
as a command holding `-p8080:80`, cannot show a change there, so `unchanged` carries the warning
`mcp.compare-masked`. A gateway running a build other than the CLI's adds `gateway.stale-build`.

**What apply promises.**

- Additive: what the manifest does not name is untouched. Removal is `swiss api DELETE`.
- Idempotent: the same manifest applied again reports only `unchanged`. Under `--no-check` that run
  sends no non-GET at all; with checks it sends only the checks
  (`apply_twice_is_all_unchanged_and_writes_nothing`).
- A stored secret is not rewritten, because the vault cannot be read back and a write rebuilds every
  MCP using it. `--rotate-secrets` rewrites those with a readable source.
- A race retries: a 409 on the vault `rev` reads it again and retries, three times.

**The order.** Secrets, then MCPs, each added with `start: false`, then every check, after every
write. A failed item does not stop the rest; what depends on it is `blocked`.

**The checks.** At most four run at once, each bounded at 75 s, and an unchanged MCP is checked too:
exit 0 says every check passed, and a rerun is how an Agent learns its fix landed.

| MCP | Check |
|---|---|
| `http`, `rest`, `mysql`, `mariadb`, `pg`, `redis`, `mongo` | `POST /api/mcpdefs/test`, 5 s. It is started as well unless it is lazy |
| `proc` | Started, tools listed, then left to the idle reaper |
| Any other type | Started, tools listed |
| OAuth | Not started or checked; `needs-input`, code `mcp.oauth`, until authorized |

A failure reads `check: {ok: false, code, error}`, with code `check.start`, `check.tools`,
`check.unreachable` or `check.timeout`. `--no-check` skips the checks and leaves a `proc` to its first
request; every other MCP still starts, and a start that fails is still reported. An MCP someone
stopped stays stopped, with the warning `mcp.left-stopped`, and a typed one is still tested.

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
    { "who": "human", "do": "store prod-db-password on the panel's Secrets page, or pipe each into swiss secret set <name> from your own shell",
      "why": "only a human has these values" },
    { "who": "agent", "do": "swiss setup apply swiss.setup.json", "why": "once that is done" }
  ]
}
```

`status` is one of `created`, `updated`, `unchanged`, `failed`, `blocked`, `needs-input`; under `plan`
the first two read `will-create` and `will-update`. A saved item whose check fails keeps its status and
sets `ok` false. `executes` names what a `proc` MCP will run, so whoever reviews a manifest can read it. A refused
manifest prints `{ok: false, mode, problems: [{path, message}]}`, and bad usage or an unavailable
gateway prints `{ok: false, error}`, with `mode` for the latter.

| Exit | Meaning |
|---|---|
| 0 | Converged; every check passed |
| 1 | At least one item or check failed; rerun after fixing |
| 2 | Bad manifest or bad usage; nothing was written |
| 3 | The gateway is unavailable and could not be started |
| 4 | Nothing failed, but items wait for a human |

**A manifest is code.** A `proc` entry names a program, and `apply` can write and run it. `executes` is
what makes that reviewable, and `plan` never runs any of it.
