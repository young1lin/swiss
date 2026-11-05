# Agent setup from the command line: design brief, draft SPEC section and delivery plan

This is the implementation brief the owner asked for before any code, not a second canonical
specification. The current-behaviour specification remains [SPEC](../SPEC.md). The draft section
in §5 and the amendments in §6 land **with the commit that makes each paragraph true**, never
ahead of it.

Status: accepted by the owner for implementation, 2026-11-05, with the decisions of §10 as written.
The delivery order of §9 is the work; this brief itself lands first.

## 1. What the owner asked (2026-11-05)

> 如果我想纯 Agent 安装这个项目，例如给这个项目编写 MCP，Secret 等等，通过 Agent 来做，应该怎么做？

> 怎么让 Agent 方便怎么来，目的很简单，就是用在 Agent 中输入配置什么的，直接操作 cli 就能设置好一堆的东西

> 那你 SKILL.md 应该包含指向一个 SETUP.md 的东西吧？要不然我输入 /swiss 不知道什么意思，也不知道怎么装，或者让 AI 读我这个项目，或者 README.md 什么的，反正打包的时候，打包成一个 zip，里面有可执行文件，和一个 SETUP.md

> 还有，swiss 要能安装的时候，让用户确定，是否安装这几个技能

> 1. 对的，skills 也要，避免 skill。2. 另外两个不变，是 tar.gz 3. 可以，你的建议可以。4. .claude 是 Claude Code 的，.agents 是其他所有的，只需要这两个装就行了

> skill 有可能随着版本更新而更新的，这个也要说清楚

So: an Agent, given a description of what the user wants, operates one CLI and a whole machine's
worth of swiss configuration lands, correctly, without the Agent learning the admin API route by
route and without a credential passing through its transcript. It finds out how from one document
that travels with the binary, and which skills land is the user's answer, not a default.

## 2. What is in the way today

The path already exists: `swiss start --no-open` plus `swiss api` configured a fresh instance from
end to end in a walk on an isolated home with dummy values. What it lacks is honesty and
ergonomics. Each row was observed on that isolated instance or read in the code named.

| # | What happens | Why it hurts an Agent |
|---|---|---|
| 1 | `POST /api/mcps` with `{"command":"npx","args":["-y","foo"]}` stores `{"type":"proc","command":"npx"}` (`adminapi.rs:213`) | The shape every MCP README prints loses its arguments and the answer looks like a success |
| 2 | A typo (`pasword`) on a mysql def is dropped; stored `{"type":"mysql","host":"127.0.0.1","lazy":true}`. `docs/spec/0005-mcp.md:153` says refused, `adminapi.rs:111` says dropped | The Agent learns nothing until a tool call fails much later |
| 3 | Proc `timeoutMs` is dropped by `build_typed_def`, though SPEC lists it, `proc.rs:79` reads it, and the panel offers it (`fields.ts:54`) | **A live bug for humans too: the panel's "Call timeout" field does nothing** |
| 4 | `POST /api/mcps` awaits the start; a proc that never speaks MCP blocks the call for the 60 s handshake deadline (`proc.rs:72`, `adminapi.rs:519`) | Two such calls ate a 90 s tool budget in the walk; a hang is the worst thing a CLI can do to an Agent |
| 5 | `swiss api` takes its body from argv only (`remote_cli.rs:1616`) | A secret on argv lands in the transcript and the process list; Windows PowerShell 5.1 strips the quotes ("the body is not JSON") |
| 6 | An unreachable http MCP answers 201 with `lifecycle:"error"`; mysql on a closed port answers started/unknown | A success exit code means nothing; every Agent must learn to dig |
| 7 | Nothing is keyed by name beyond MCPs: two connections named `c1` coexist under two UUIDs | "Run it again" duplicates; no idempotence to lean on |
| 8 | `PUT /api/plugins/{id}/config` replaces the whole row and skips the CAS check when `revision` is absent (`host/api.rs:212`) | Adding one job or one provider means read, merge, write, or losing the rest |
| 9 | A literal password or env value is accepted and masked on echo; `Bearer ${secret://x}` is masked on read-back too (`mask.rs:326`) | The rule "references, never literals" is not enforced, and a diff cannot tell whether a header changed |
| 10 | The only ways to give a secret are `PUT /api/secrets` with the value inside the body, or a human typing the name into the panel | Either the Agent sees the value or the human gets no guidance |
| 11 | `swiss export` prints plaintext config and is the obvious word to reach for | An Agent asking "what is configured?" can pour credentials into its transcript |
| 12 | The `swiss` skill scopes itself to "installation, updates, and non-secret local checks", says nothing for a bare `/swiss`, and its macOS note is stale against `secure/key.rs:28` | Nothing tells an Agent how to configure anything, and a user typing `/swiss` learns nothing |
| 13 | The archive holds no instructions; the only ones are in a skill, which exists only after `swiss skill install` has run | Someone holding the zip, or an Agent told to read the project, has nothing to start from |
| 14 | `swiss skill install` writes all three skills into every client directory without asking (`skill_install.rs:176`), and `swiss-memory` is model-invoked, so it loads in that user's sessions unasked | The user never chose any of it |

## 3. What it is, and is not

**It is one declarative file and one command.** The Agent writes what the user wants; swiss
converges on it and reports per item, in JSON, what happened and what only a human can finish.

```sh
swiss setup schema                   # what a manifest may say, from this binary
swiss setup show                     # what this gateway already has, no values
swiss setup plan  swiss.setup.json   # what would change; writes nothing, starts nothing
swiss setup apply swiss.setup.json   # converge; running it again is always safe
```

Beside it, the two things a file must not do: `swiss secret set` and `swiss secret ask` move a
value without any transcript seeing it, and `swiss connect` wires an AI client to the gateway
without printing a token.

**One document is the way in.** `SETUP.md` tells an Agent, or a person, how to install swiss, ask
which skills to add, and configure it. It is in the release archive beside the executable, at the
repository's root, beside the `swiss` skill once installed, and inside the binary:

| Where the user starts | How the Agent reaches `SETUP.md` |
|---|---|
| Holding the extracted zip | It is next to the executable |
| "Read this project" | At the root; `README.md` and `AGENTS.md` both point at it |
| Nothing but a request to install swiss | `releases/latest/download/SETUP.md`, a release asset |
| Types `/swiss` | `SKILL.md` points at the copy beside it |
| swiss on PATH, nothing else | `swiss setup guide` prints the one this binary shipped with |

**It is not:** a new admin surface (it calls the existing `/api/*` through the CLI key), an MCP for
administering swiss (that would hand the MCP bearer the admin boundary), daemon state (it lives
and dies with the CLI process), or a sync tool (nothing is ever deleted by a manifest).

## 4. The choices, with reasons

- **A manifest, not `swiss mcp add` and friends.** A dozen resource verbs would double `§host.cli`
  and still need ordering by the Agent. One file makes dependencies swiss's problem and makes a
  rerun the recovery from any failure. A single MCP is a three-line manifest on stdin, so the small
  case stays small.
- **The orchestration lives in the CLI (`src/setup_cli/`), not behind a `POST /api/setup`.** The
  `env:` and `dotenv:` sources belong to the caller's environment, not the daemon's. The root crate
  already reuses the pure validators, and no new route means no new security surface. The daemon
  gains no state and no idle cost.
- **JSON only.** `serde_json` is here already and every Agent writes it. YAML or TOML would be a
  dependency for a human reader who is not the audience.
- **Additive and keyed by name.** Absence never deletes; removal goes through `swiss api`, so a
  stale manifest can never remove anything. An entry *is* the whole definition of that entry; a
  plugin block is a merge patch, because its row holds what other tools wrote.
- **Refusal over silent repair.** Unknown fields and literal credentials fail the whole file before
  anything is written, with every problem named at once, so the Agent fixes them in one turn.
- **A secret's source travels, its value does not.** The manifest says where the value is; the CLI
  reads it and puts it in the vault. So a manifest is safe to keep in a repository.
- **Nothing hangs.** Writes never start a child; each check has a deadline; checks run concurrently;
  `secret ask` returns at once unless told to wait. Agents have tool budgets, and a rerun is free.
- **`swiss setup show`, not `swiss setup export`.** `swiss export` is the plaintext escape (§host.cli);
  a second command spelled the same way and meaning the opposite is a credential leak waiting.
- **One `SETUP.md`, not a fourth skill.** The owner's shape: `/swiss` stays the one entrance and its
  `SKILL.md` stays short, pointing at a document that also works for someone who has no skill yet. A
  document needs no installing, so it has no chicken-and-egg. The cost is that "add an MCP to swiss"
  does not load anything by itself: the user types `/swiss` or the Agent reads `swiss --help`.
- **Consent is enforced, not just written down.** `swiss skill install` with no names asks at a
  terminal and refuses without one, so an Agent that skips the question hits a wall that tells it to
  ask. An update refreshes what is there and never adds what the user left out; absence is the record
  of the answer, so swiss keeps no list of declined skills.
- **Rejected:** an admin MCP; a value flag on `secret set`; `--prune`; a trust-on-first-use field
  for SSH; free-text hints in the `secret ask` link (agent-written text inside a trusted page); a
  hidden terminal prompt (needs FFI per platform, and the panel already asks better).

## 5. Draft SPEC section

To be added to `docs/spec/0004-host.md` after §host.cli, with its anchor-map line.

### Draft section `host.setup` — Configuring from the command line

An Agent or a script configures a machine by describing it. `src/setup_cli/` reads a JSON manifest
and drives the same `/api/*` routes the panel uses, signed with the CLI key (§host.session). It
holds no state and adds nothing to the daemon.

**Commands.**

```text
swiss setup guide                        print the SETUP.md this binary shipped with
swiss setup schema [section]             what a manifest may say
swiss setup show                         this gateway's state as a manifest, without values
swiss setup plan  <file|->               what apply would do; writes nothing, starts nothing
swiss setup apply <file|-> [--no-check] [--rotate-secrets]
swiss secret set <name> [--from <source>]  value from the source, or piped stdin
swiss secret ask <name>... [--wait <seconds>]
swiss connect <claude|codex|cursor> [--mcp <name>]... [--replace]
```

`-` reads stdin. A file or stdin may carry a BOM or be UTF-16, which is what Windows PowerShell 5
writes. All of them print one JSON document on stdout, progress goes to stderr, and none prompts.
Each starts the gateway without a browser if it is down, and reports that it did, except
`setup guide`, which needs no gateway, and `setup schema`, which needs one only for `plugins.<id>`.

**The manifest.**

```json
{
  "version": 1,
  "machine": { "path": true, "autostart": true, "skills": ["swiss", "swiss-memory"] },
  "secrets": {
    "github-token": "env:GITHUB_TOKEN",
    "glm-key": "dotenv:.env#ZAI_API_KEY",
    "deploy-key-pass": "file:~/.config/acme/pass",
    "prod-db-password": "ask"
  },
  "plugins": {
    "llm": { "config": { "providers": { "glm": {
      "style": "anthropic",
      "baseUrl": "https://open.bigmodel.cn/api/anthropic",
      "apiKey": "${secret://glm-key}"
    } } } },
    "jobs": { "config": { "definitions": { "backup": {
      "trigger": { "kind": "cron", "expression": "0 3 * * *" },
      "action": { "type": "process.exec", "input": { "program": "restic", "args": ["backup", "."] } }
    } } } }
  },
  "tunnels": {
    "connections": { "bastion": {
      "host": "bastion.example.com", "username": "deploy", "authType": "key",
      "keyPath": "~/.ssh/id_ed25519", "passphrase": "${secret://deploy-key-pass}",
      "hostKey": "SHA256:..."
    } },
    "rules": { "prod-db": {
      "connection": "bastion", "localPort": 13306,
      "targetHost": "10.0.0.5", "targetPort": 3306, "start": true
    } }
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
  "remote": { "targets": { "gpu": { "endpoint": "bastion", "root": "/srv/gpu" } } },
  "clients": { "claude": {}, "codex": { "mcps": ["github"] } }
}
```

Every section is optional. A client's own `.mcp.json` is a valid manifest: `mcpServers` and
`servers` are accepted for `mcps`, and entries normalise as §mcp.import does (`command` + `args`,
a `url` becomes `http`, an entry pointing at this gateway is skipped).

| Section | Identity | Meaning |
|---|---|---|
| `machine` | - | `path` and `autostart` true or false, absent leaves alone. `skills` names the skills the user chose (§host.cli); anything else is refused, `true` included, and it never removes one |
| `secrets` | name | Source of the value. `env:VAR`, `file:PATH`, `dotenv:PATH#KEY` or `ask`. Relative paths resolve against the manifest's folder, or the working directory for stdin |
| `plugins` | plugin id | `{enabled?, config?}`. `config` is a merge patch over the plugin's row, so `plugins.jobs.config.definitions.backup` adds one job; arrays replace. Validated by the plugin's own `validate_config` (§host.config) |
| `tunnels.connections` | name | The whole `conn_input` definition |
| `tunnels.rules` | name | The whole `rule_input` definition, `connection` naming a connection, `start: true` meaning it should be running |
| `mcps` | name | The whole MCP definition (§mcp.defs), keys per type |
| `remote.targets` | id | `{endpoint, root, caps?}` |
| `clients` | client | `{mcps?}` as `swiss connect`; absent `mcps` means the MCPs this manifest names, `"*"` means all |

**What apply promises.**

- *Additive.* An item absent from the manifest is untouched. `null` is refused everywhere; removal
  is `swiss api DELETE`.
- *Idempotent.* The same manifest applied twice reports only `unchanged` the second time. It writes
  nothing and rebuilds nothing, compared through `build_def` on one side and the masked read-back
  on the other.
- *Secrets are not rewritten.* The vault cannot be read back and a write rebuilds every MCP using the
  secret, so an existing name stays `unchanged`. `--rotate-secrets` rewrites those with a readable
  source.
- *Ambiguity stops, not guesses.* Two connections or rules sharing a name fail that item, naming the
  ids; nothing is merged.
- *Races retry.* A 409 on a vault `rev` or a plugin `revision` re-reads, re-merges and retries, three
  times. Every plugin write carries its revision.
- *One build.* A gateway running a build other than the CLI's adds the warning `gateway.stale-build`.

**Refused before anything is written** (exit 2, every problem listed, with its path):

- an unknown field, with the nearest known one (`mcps.db.pasword: unknown field; did you mean
  password?`);
- a credential literal in any credential-named field, header, env key, URL password or command
  argument, recognised by the `mask.rs` vocabularies and `swiss_memory::secrets::scan`. The message
  never quotes the value, and says what to write: a `secrets` entry and a `${secret://name}`;
- the mask sentinel, which only `setup show` can have produced;
- `null`, a `version` other than 1, a bad secret or MCP name, a rule naming a connection that is
  neither in the manifest nor on the gateway, or a source syntax this build does not know.

A `${ENV_VAR}` reference is accepted with the warning `ref.env-unverifiable`: the CLI cannot see the
daemon's environment, and a missing variable expands to empty (§host.refs). `${secret://name}` is the
form to write.

**The order.** Machine, secrets, plugins (config, then enable), connections, MCPs, rules, targets,
clients. An MCP is added with `start: false`; every check runs after every write, so a mysql MCP
behind a tunnel is checked after its tunnel came up. A failed item does not stop the rest; what
depends on it is `blocked`.

**Checks** (skipped by `--no-check`, except that non-proc MCPs still start). Run concurrently, each
bounded:

| Item | Check | Bound |
|---|---|---|
| Typed MCP | `POST /api/mcpdefs/test`, which costs at most a handshake or a plain GET | 5 s |
| Proc MCP | Start, then list tools. The child is then left to the idle reaper (§mcp.proc) | the 60 s handshake |
| OAuth MCP | Not checked; reports `needs-input` until authorized | - |
| Connection | `POST /api/tunnels/connections/{id}/test`. An unpinned host key is `needs-input`, with the fingerprint | its own |
| Rule | Started, and its state read | its own |
| LLM provider | `POST /api/llm/providers/models`, a listing. Never `providers/test`, which is billed | its own |

**The output.** One document, on success and on failure alike. It never holds a credential, a
token, a one-time link or a secret's value.

```json
{
  "ok": false,
  "mode": "apply",
  "gateway": { "port": 19999, "started": true, "build": "6a01f5eb" },
  "summary": { "created": 5, "updated": 0, "unchanged": 2, "failed": 1, "blocked": 1, "needsInput": 1 },
  "items": [
    { "kind": "secret", "name": "github-token", "status": "created" },
    { "kind": "secret", "name": "prod-db-password", "status": "needs-input", "code": "secret.ask" },
    { "kind": "mcp", "name": "prod-db", "status": "blocked", "by": ["secret:prod-db-password"] },
    { "kind": "mcp", "name": "playwright", "status": "created",
      "executes": "npx -y @playwright/mcp@latest", "check": { "ok": true, "detail": "21 tools" } },
    { "kind": "mcp", "name": "github", "status": "created",
      "check": { "ok": false, "code": "check.unreachable", "error": "connection refused" } }
  ],
  "warnings": [],
  "next": [
    { "who": "human", "do": "swiss secret ask prod-db-password", "why": "no source for this secret" },
    { "who": "agent", "do": "swiss setup apply swiss.setup.json", "why": "after the secret is set" }
  ]
}
```

`status` is one of `created`, `updated`, `unchanged`, `failed`, `blocked`, `needs-input`; under
`plan` the first two read `will-create` and `will-update`. A saved item whose check fails keeps its
status and sets `ok` false. `executes` names what a `proc` MCP will run, in `plan` too; a job
patched into `plugins.jobs` is listed the same way, read from its `process.*` action.

| Exit | Meaning |
|---|---|
| 0 | Converged; every check passed |
| 1 | At least one item failed or one check failed; rerun after fixing |
| 2 | Bad manifest or bad usage; nothing was written |
| 3 | The gateway is unavailable and could not be started |
| 4 | Nothing failed, but items wait for a human |

**`swiss secret set`** takes the value from `--from <source>` or piped stdin, one trailing newline
trimmed. There is no value flag. A terminal stdin is refused rather than echoing the value, and the
refusal names `swiss secret ask`. An empty value is refused. `dotenv:` reads `KEY=VALUE`,
`export KEY=VALUE`, quotes and `#` comments, with no interpolation; a parse error gives a line
number and never the line.

**`swiss secret ask`** is how a human gives what an Agent must not see. It mints a login ticket
(§host.session), opens `#secrets?ask=a,b` in the browser through the same call `swiss open` uses,
and never prints the link. The Secrets page opens with one row per missing name, the name fixed and
the value focused; it does not submit by itself. The command returns at once with exit 4, or with
`--wait <seconds>` polls `GET /api/secrets` until every name exists. With no desktop to open, it says
so and names the two other ways: forward the port and open the panel, or pipe the value into
`swiss secret set` from the human's own shell.

**`swiss connect`** registers MCPs with an AI client and never prints the token. It reuses the token
labelled `client-<client>`, minting it if absent. Claude Code goes through `claude mcp add` (so
`$CLAUDE_CONFIG_DIR` is honoured), Codex through a `[mcp_servers.<name>]` block in
`$CODEX_HOME/config.toml` edited as text, and Cursor through a merge of `~/.cursor/mcp.json`. An
existing entry of that name is `unchanged` unless `--replace`. `next` always says to restart the
client: a running one does not re-read its MCP list.

**A manifest is code.** `apply` can write and run a `proc` command or a job. `executes` is what makes
that reviewable; `SETUP.md` tells an Agent to show it to the user before applying a manifest it did
not write. `plan` never runs any of it.

**`SETUP.md`** is the one set of instructions for installing and configuring swiss, kept at the
repository's root and embedded with `include_str!`. In order, it covers: what swiss is; how to tell
what state this machine is in; installing (approval, download, checksum, PATH); asking which skills
to add; updating (replace the executable, then refresh the skills that are there, because they ship
inside it and may have changed); the configuration loop; the rules that are never bent; and what each exit code and
`needs-input` asks of the human. Field lists stay in `swiss setup schema`, so the document cannot go
stale against the binary. `the_setup_guide_names_every_command_and_exit_code` holds it to the commands.

## 6. Amendments to other sections

- **§host.cli.** Add `setup …`, `secret …`, `connect …` and `api <METHOD> <path> [json |
  --body-file <path|->]`. The `skill install` paragraph becomes:

  ```text
  swiss skill list [--json]        each skill: what it does, whether it loads by itself, where it is,
                                   and whether that copy still matches this binary
  swiss skill install              at a terminal, asks per skill, default no;
                                   with no terminal, refuses with exit 2 and the list
  swiss skill install <name>...    exactly these
  swiss skill install --all        every shipped skill, said out loud
  swiss skill install --installed  refresh what is already there; never adds. What an update runs
  ```

  A skill goes to exactly two directories: `.claude` for Claude Code (`$CLAUDE_CONFIG_DIR/skills`
  when that is set, because Claude Code then reads nothing under `~/.claude`) and `.agents` for every
  other client. The CLI, `GET /api/skills` and `POST /api/skills/install` share one list, ending the
  old split where the CLI wrote `.cursor` and the panel read `.codex`. Copies an older build left in
  `~/.cursor/skills` or `~/.codex/skills` are not touched and not refreshed. Whether a skill loads by
  itself is read from its `disable-model-invocation` line, not written twice. `POST
  /api/skills/install` also writes `SETUP.md` for the `swiss` skill.

  Skills ship inside the binary, so a new version may change them, and a copy on disk does not change
  by itself. `skill list` marks a copy that differs from this binary's as `outdated`. Updating swiss
  is therefore two steps: replace the executable, then `swiss skill install --installed`. `SETUP.md`,
  the `swiss` skill and what `swiss update` prints all say so. Nothing refreshes a skill behind the
  user's back: no gateway boot, no panel load. A refresh replaces `SKILL.md` and leaves anything else
  the user put in that directory.
- **§release (`0017-release.md:40`).** Each archive also holds `SETUP.md` at its root. The release
  publishes `SETUP.md` as an asset of its own, and `SHA256SUMS` covers it. The ADR entry at
  `0018-decisions.md:47` names it among the contents. Windows stays `.zip`; Linux and macOS stay
  `.tar.gz`, which keeps the executable bit with no extra tooling.
- **§mcp.defs (`0005-mcp.md:153`), keys per type.** The text stands, and the code comes to it. `build_def` refuses a
  key outside its type's list, by name, from one table that `setup schema` also prints. Proc gains
  `args`, joined into `command` by `join_command` as an import does. Proc keeps `timeoutMs`. The route
  reads `start` (default: as `enabled`), never stored.
- **§host.config.** `PUT /api/plugins/{id}/config?dryRun=1` validates and returns the warnings without
  persisting; `plan` uses it.
- **§mcp.defs, masking.** `mask_record` keeps a value of the exact shape `<scheme> <whole reference>`
  (`Bearer ${secret://x}`) instead of masking it: it holds no credential, and the panel and the diff
  both need to see it. `is_env_ref` itself stays as `tests/adminapi/vault.rs:261` pins it.
- **§panel.** The Secrets page reads `?ask=` in its hash route.

Skills and docs, outside SPEC:

- **`SETUP.md`**, new, at the root. The rules it carries: never print `swiss token` or `swiss creds`,
  never run `swiss export`, no value in a manifest or on argv, show `executes` for a manifest you did
  not write, and never pick the skills for the user. The skill step shows `swiss skill list` and asks;
  installing none is a valid answer, and swiss works without them.
- **`swiss` skill** keeps `disable-model-invocation: true` and shrinks to an entrance. On a bare
  `/swiss` it runs `swiss --version` and `swiss status`, says in a few lines what swiss is and what it
  found, offers install or update, configure, and check, and does nothing until the user picks. For
  any of them it reads `SETUP.md` beside itself, falling back to `swiss setup guide`, then to the
  release asset. It keeps the boundaries: act only on request, loopback only, never print a token. The
  install steps and their stale macOS note move out; the checksum test moves to `SETUP.md`.
- **README** gains a line to paste into an Agent: "Read
  `https://github.com/young1lin/swiss/releases/latest/download/SETUP.md` and install swiss for me." Its
  skills paragraph stops saying they all install by default.
- **`AGENTS.md`** gains one line near the top: to install or configure swiss rather than change it,
  read `SETUP.md`. That is what an Agent told to read this project opens first.
- **`connect.ts`** loses the comment "Codex has no one-line add", which is no longer true.

## 7. Against the product properties and the load-bearing rules

- **Ruthlessly small.** No new dependency, no daemon state, no idle cost: the code runs only inside
  the CLI process. Binary growth is reported in the commit that adds it.
- **Plugin-shaped.** A plugin with a config schema is configurable with no setup code, because
  `plugins` is generic. MCPs, tunnels, secrets and remote targets have their own stores, and the root
  crate that names them is the composition layer. No edge between subsystem crates, no match arm in
  the host.
- **Hot-pluggable.** Every write takes an existing hot path. Nothing restarts the gateway.
- **Three ways to bring a tool in.** `mcps` covers stdio, HTTP and every compiled adapter.
- **Loopback and the admin gate.** Unchanged. The CLI signs with its key; no `/api` exemption; the
  bearer is still refused there.
- **References, never literals.** Stricter in a manifest than anywhere else. The panel and `swiss api`
  still accept a typed literal and mask it (§10.3).
- **`http`/`rest` have no ping.** A check is one bounded test the user asked for by applying, through
  the existing route; `--no-check` skips it. There is no recurring probe.
- **A proc is lazy.** A manifest never makes one eager unless it says `lazy: false`. `--no-check` leaves
  it idle; a check wakes it once and the reaper takes it.
- **Sealed envelopes.** Untouched.

## 8. Acceptance tests

Written first, each citing its rule. The core is driven through a trait over the admin calls, so it
runs against the axum app via `oneshot`, with no real port and no real sleep (as `cli.rs`'s `Ops`
does). `tests/setup_e2e.rs` spawns the real binary on a private `SWISS_HOME` and port, as
`tests/boot_order.rs` does.

| Test | File | Fails before, passes after |
|---|---|---|
| `an_unknown_mcp_key_is_refused_by_name` | `tests/adminapi/mcp_keys.rs` | Row 2 |
| `every_panel_form_key_is_accepted` | same | Reads `fields.ts`; refusal cannot break a form |
| `args_join_into_the_command_like_an_import` | same | Row 1 |
| `a_proc_keeps_its_call_timeout` | same | Row 3 |
| `start_false_adds_without_starting` | same | Row 4: a proc that never handshakes answers at once |
| `a_scheme_and_a_reference_survive_masking` | `crates/swiss-host/src/mask.rs` | Row 9 |
| `api_body_file_and_stdin_reach_the_gateway` | `src/remote_cli.rs` | Row 5, BOM and UTF-16 included |
| `a_manifest_lists_every_problem_at_once` | `src/setup_cli/` | Nothing written, exit 2 |
| `a_literal_credential_is_refused_without_quoting_it` | `src/setup_cli/` | Each credential shape; the value is absent from the output |
| `a_client_mcp_json_is_a_manifest` | `src/setup_cli/` | `.mcp.json` applies unchanged |
| `apply_twice_is_all_unchanged_and_writes_nothing` | `src/setup_cli/` | Every section; the second run issues no write |
| `plan_writes_nothing_and_starts_nothing` | `src/setup_cli/` | Row 4; store revisions and registry unchanged |
| `a_plugin_patch_keeps_the_rest_of_the_row` | `src/setup_cli/` | Row 8 |
| `a_conflict_rereads_and_retries` | `src/setup_cli/` | A 409 on `rev` and on `revision` |
| `a_shared_connection_name_stops_that_item` | `src/setup_cli/` | Row 7 |
| `a_missing_secret_blocks_its_users_and_exits_4` | `src/setup_cli/` | Then converges once the secret exists |
| `a_failed_check_exits_1_with_the_item_saved` | `src/setup_cli/` | Row 6 |
| `no_secret_value_ever_reaches_the_output` | `tests/setup_e2e.rs` | Sentinel values from each source are absent from stdout, stderr and the daemon log |
| `no_token_or_link_reaches_the_output` | `tests/setup_e2e.rs` | `secret ask` and `connect` with fake client binaries on `PATH` |
| `a_terminal_stdin_is_refused` | `src/setup_cli/` | `secret set` never echoes |
| `every_plugin_with_a_schema_refuses_an_unknown_key` | `tests/` | A generic guarantee; memory's validator is not yet proven |
| `the_setup_guide_names_every_command_and_exit_code` | `src/skill_install.rs` | As the memory skill's sync test does |
| `the_swiss_skill_points_at_setup_md` | `src/skill_install.rs` | `SKILL.md` names `SETUP.md`, keeps its frontmatter pins; the checksum pin moves to `SETUP.md` |
| `both_install_paths_write_setup_md_beside_the_skill` | `src/skill_install.rs` | `swiss skill install swiss` and the panel's route alike; `install_selected_in` writes `SKILL.md` alone today |
| `the_release_carries_setup_md` | `src/skill_install.rs` | Reads `build.yml`: the archive step, the asset, the checksum list |
| `no_terminal_and_no_names_refuses_and_installs_nothing` | `src/cli.rs` | Row 14: exit 2, the list printed, every skill directory untouched |
| `a_terminal_prompt_defaults_to_no` | `src/cli.rs` | Enter on every question writes nothing; only a yes writes that skill |
| `naming_skills_installs_exactly_those` | `src/skill_install.rs` | An unknown name refuses the whole call |
| `installed_refreshes_and_never_adds` | `src/skill_install.rs` | A declined skill stays absent through an update, per directory |
| `skills_go_to_exactly_the_two_directories` | `src/skill_install.rs` | `.claude` (or the config dir) and `.agents`; nothing in `.cursor` or `.codex`, which the panel route refuses too |
| `list_says_which_skills_load_by_themselves` | `src/skill_install.rs` | `swiss-memory` yes, the other two no, read from the frontmatter |
| `list_marks_a_copy_older_than_this_binary` | `src/skill_install.rs` | A changed `SKILL.md` reads `outdated`, the shipped text `current`; `--installed` brings it back |
| `a_manifest_skills_true_is_refused` | `src/setup_cli/` | Only named skills, and only those land |
| `the_ask_link_prefills_without_submitting` | `crates/swiss-panel/panel/test/` | Panel behaviour, both locales |

Live, on an isolated instance (never 19999): a fresh home converged by one real manifest with PowerShell
5.1, pwsh 7 and Git Bash; the `secret ask` page walked in a real browser (panel proof of life); and
`swiss connect` against the real `claude` and `codex` on this machine, on a scratch config directory.

## 9. Delivery order

One commit per item; each carries its code, its tests and its slice of the SPEC text. Gates are the
usual, serially: `cargo test --workspace -j 4`, clippy, and `npm run check` when the panel moves.

1. **Honest MCP definitions.** One key table, unknown keys refused, `args`, proc `timeoutMs`, `start`.
   Fixes the panel's dead field. Worth shipping by itself.
2. **A reference survives masking.** `mask_record`, and the panel's display of it.
3. **`swiss api --body-file` and `swiss secret set`.** Text decoding split out of `stdin_text`, plus the
   sources. From here an Agent can move a value safely, by hand.
4. **Choosing skills.** `swiss skill list`, the refusal and the prompt, names, `--all`, `--installed`.
   Stands alone, and `machine.skills` needs it.
5. **Manifest, validation, `setup schema`, `setup plan`** for `secrets` and `mcps`.
6. **`setup apply`** for those two, with the output contract and checks. **The main path works here.**
7. **`plugins`**, with `dryRun`. LLM, jobs and memory all arrive at once.
8. **`tunnels`, `remote`, `machine`.**
9. **`setup show`.**
10. **`secret ask`** and the panel's deep link.
11. **`swiss connect`** and `clients`.
12. **`SETUP.md` and every way to it.** The document, `setup guide`, both install paths writing it, the
    `swiss` skill as an entrance, `build.yml`'s archive, asset and checksum, README and `AGENTS.md`.

Items 5 to 11 live in `src/setup_cli/`, a directory from the start: `manifest`, `sources`, `refuse`,
`plan`, `apply`, `check`, `report`.

**Reach.** An Agent installing from GitHub Releases gets none of this until a release carries it:
v0.1.4 predates all of it. It does ship `swiss api` and the routes §2 used, though that walk ran this
branch's build, not v0.1.4. Cutting a release is §10.7.

**Facts still to confirm while building:** the current field names of Codex's `config.toml` for HTTP
headers (the panel's snippet may be behind; item 10 proves them against the installed client), and
whether every plugin refuses an unknown key (the test above).

The release asset answers 404 until the first release that carries it, so the README line lands with
that release, not before it.

**Later, not now:** `--prune`; a `groups` section; project-level `swiss memory init` in a manifest;
refusing typed literals on the panel and API; a hidden terminal prompt; choosing per client directory
on the command line (the panel can already); removing a skill from the command line.

## 10. The owner's decisions

Accepted as recommended on 2026-11-05. Items 8 and 9 and the two-directory rule were answered
explicitly; Linux and macOS archives stay `.tar.gz`, and `skills/` stays in every archive.

1. **A manifest as the one entrance**, not per-resource verbs like `swiss mcp add`. *Recommended:*
   manifest; a single MCP is three lines on stdin.
2. **`POST /api/mcps` starts refusing unknown keys**, which follows SPEC and overturns the code's comment.
   Anything sending extra keys breaks loudly. *Recommended:* refuse, since master is unreleased and the
   panel is pinned by a test.
3. **Literal credentials are refused in a manifest with no override**, while the panel and `swiss api`
   keep accepting and masking them. *Recommended:* yes for now; tightening those is a separate change
   because existing configurations hold literals.
4. **`swiss connect` and `clients` in this round**, which write other programs' config files.
   *Recommended:* in, as the second-last commit, because without them the Agent's job ends with a token
   it must not print.
5. **No fourth skill; `/swiss` points at `SETUP.md`.** The owner's shape, replacing the earlier
   `swiss-setup` idea. The cost: "add an MCP to swiss" loads nothing by itself. *Recommended:* yes; a
   model-invocable skill can be added later without changing anything here.
6. **SSH host keys are pinned in the manifest or confirmed by a human**, with no trust-on-first-use field.
   *Recommended:* yes; that prompt is the only thing standing between a tunnel and a man in the middle.
7. **Cut a release after item 12**, as the only way Agents installing from Releases see this.
   *Recommended:* yes, when you judge the branch ready; nothing here depends on it.
8. **What else stays in the archive** beside the executable and `SETUP.md`. `LICENSE`, `NOTICE` and
   `THIRD_PARTY_NOTICES.md` must stay: the licence terms require them. *Recommended:* keep `skills/` too,
   because it is how someone reads a skill before running an unsigned executable, and it is what they
   are being asked to choose. Dropping it is one line in `build.yml` if the owner wants the zip tidier.
9. **Bare `swiss skill install` with no terminal now refuses**, where it installed all three. Any script
   or habit relying on that needs `--all` or `--installed`; no script in this repository calls it, and
   the owner's own "reinstall after a skill edit" step becomes `--installed`. *Recommended:* refuse,
   since a default that installs everything is what the owner asked to end.
