# 03 — Skill seats: every installed copy is known, shown and refreshable

Implementation brief, not SPEC. The SPEC amendments in §8 land in the commit that makes each
paragraph true. Shared rules: [00-handoff.md](00-handoff.md). Depends on 01. Brief 04/U2 calls
the refresh this brief builds, so 03 lands before U2.

## 1. The problem

The owner installs skills with `swiss skill install` into Claude Code and into `~/.agents`, and
wants a way for those copies to **follow an update**. Three gaps today:

- **P1 — copies go stale silently.** A new binary carries new skill text; the copies on disk
  stay old until someone remembers `swiss skill install --installed`. `swiss skill list` marks
  them `outdated`, but nothing prompts the user to look.
- **P2 — the panel looks in the wrong Claude directory.** `.claude` resolves to
  `$CLAUDE_CONFIG_DIR/skills` when that is set (`Places::root_dir`). The CLI runs in the
  agent's shell, so it sees the agent's `CLAUDE_CONFIG_DIR` (on the owner's machine, a directory other than `~/.claude`).
  The panel's `/api/skills` runs in the **daemon**, which has whatever environment `swiss start`
  or the sign-in Run key gave it — usually no `CLAUDE_CONFIG_DIR`, so the panel shows
  `~/.claude/skills` as "absent" while the real copy sits elsewhere. `setup_cli/machine.rs:19`
  already names this ("the daemon's `CLAUDE_CONFIG_DIR` need not be the Agent's").
- **P3 — there can be several Claude config directories.** Multiple accounts are normal
  (memory: "Agents run on several homes"); a refresh from one shell refreshes only the one its
  env points at.

## 2. Facts (verified 2026-11-12)

- `src/skill_install.rs`: `ROOTS = [".claude", ".agents"]`; `Places {home, claude_config}`;
  `of_this_user()` reads `USERPROFILE`/`HOME` and `CLAUDE_CONFIG_DIR`; `status()`,
  `install_one(name, root)` (stages `.SKILL-<hex>.tmp`, renames with `retry_busy`, then the
  companions), `install_named`, `refresh_installed()` (Outdated → replaced, Absent → left,
  Unreadable → failed; never adds).
- **The text depends on the kind**: `rendered(name, root)` rewrites the remote skill's body for
  `.claude` only (BRIEF_STEP/BRIEF_TOOLS). So a seat must carry its kind, not just a path.
- `CopyState` is a byte compare of `SKILL.md` **and** the companions (`SETUP.md`, `REFERENCE.md`).
- CLI: `src/cli.rs:842` `skill_command(p, io, places, ask)` takes `&Places` (tests pass a
  tempdir one). `skill_refresh` at :949. `SKILL_REFUSED = 2`.
- Daemon: `/api/skills` and `/api/skills/install {name, root}` (`src/adminapi.rs:702-727`) call
  `skill_install::status()` / `install_selected()`, which build `Places::of_this_user()` per
  request. The integration tests (`tests/adminapi/machine.rs:82-116`) run with the sandboxed
  home and assert every path stays under `swiss_core::paths::data_dir()`.
- `swiss_core::paths::data_dir()` is `$SWISS_HOME` or `<home>/.swiss`, read fresh each call.
- SPEC §host.cli (`docs/spec/0004-host/04-command-line.md:86-132`) says a skill goes to "exactly
  two directories", and "No gateway boot and no panel load refreshes a skill behind the user's
  back". **That second rule stays.** Every refresh in this brief is an explicit act.

## 3. The design: a seat ledger

A **seat** is one skills directory of one kind:

```json
{ "schema": 1,
  "seats": [
    { "kind": "claude", "dir": "C:\\Users\\jdoe\\.claude-work\\skills", "addedAt": "2026-11-12T06:00:00Z" },
    { "kind": "agents", "dir": "C:\\Users\\jdoe\\.agents\\skills",            "addedAt": "2026-11-12T06:00:00Z" }
  ] }
```

- Stored at `data_dir().join("skill-seats.json")`. Plain JSON, not sealed: it holds directory
  paths the user already has, no secret. Written atomically (temp + rename, the module's
  existing staging pattern). A missing file is an empty ledger; an unreadable or unparseable one
  is reported (CLI stderr, panel note) and treated as empty for reading. **Never overwrite an
  unparseable ledger** — refuse writes until it is fixed or removed, and say where it is.
- `kind` is `"claude"` or `"agents"` and maps to today's roots `.claude` / `.agents` for
  `rendered()`.
- `dir` is the absolute skills directory, stored as given after `std::path::absolute` and
  trailing-separator trimming. Compare seats case-insensitively on Windows, exactly elsewhere.

**The seat list** a process works with = the **default seats** of its own env (today's two:
`root_dir(".claude")`, `root_dir(".agents")`) ∪ the ledger, de-duplicated by (kind, dir), default
seats first. Each seat in a status answer says where it came from: `"source": "env" | "ledger" | "both"`.

**Recording.** Every successful `install_one` records its seat in the ledger (idempotent). So
the first `swiss skill install` run from the agent's shell teaches the daemon where that
agent's Claude directory is — this alone fixes P2 for every user who ever installed from the
CLI, with no setting to find. `refresh_installed` records nothing new (it only touches seats
already known).

**Absent seats.** A ledger seat whose `dir` no longer exists shows as `missing` (a new
`CopyState::Missing`, or a seat-level `exists:false` — pick one and keep it in the JSON contract)
and offers Forget. It is never created back by a refresh.

## 4. Behaviour by surface

### 4.1 CLI

- `swiss skill list` prints one block per seat (kind, dir, source) with each skill's state, and
  `--json` carries the same seats array.
- `swiss skill install <names>|--all|(prompt)` installs into the **default seats** only (today's
  two directories) — unchanged — and records them.
- `swiss skill install --installed` refreshes **every seat** (env ∪ ledger): Outdated copies are
  replaced, Absent stays absent, Missing seats are skipped with a note. Never adds. Exit 0 when
  nothing failed, 1 when any copy failed (stderr names each). This is what U2 and `deploy.ps1`
  call.
- `swiss skill seat add <claude-config-dir> [--kind claude]` records `<dir>/skills` as a claude
  seat (the user names the **config** directory, the same thing they put in `CLAUDE_CONFIG_DIR`;
  accept a path that already ends in `skills` too and do not double it). `--kind agents` takes a
  skills directory as is. The directory must exist and be a directory; the seat's skills
  subdirectory may not exist yet (it is created by the next install, not by `seat add`).
- `swiss skill seat forget <dir>` removes a ledger seat. Forgetting a default seat is refused
  with a note (it comes back from the env on the next call anyway).
- `swiss status` gains one line **only when** something is outdated:
  `skills: 2 outdated copies (swiss skill install --installed)`. Counted over every seat. This is
  the nudge for P1. Silent when everything is current or nothing is installed.

### 4.2 Gateway API

- `GET /api/skills` → `{seats: [{kind, dir, source, exists}], skills: [{name, description, loadsByItself, locations: [{kind, root, dir, path, installed, state, error?}]}]}`.
  Keep `root` on each location (`.claude` / `.agents`) so the existing test and any older panel
  still read it; `locations` gains one entry per seat instead of exactly two.
- `POST /api/skills/install` accepts `{name, root}` (today's body, default seat of that kind)
  **or** `{name, dir}` where `dir` must equal a known seat (env ∪ ledger) — otherwise 400
  `skills.unknownDir`. `deny_unknown_fields` stays, so `path` and anything else is still 400.
- `POST /api/skills/refresh` → the same refresh as `--installed`, answers
  `{replaced: [path], failed: [message], skills: <status>}`.
- `POST /api/skills/seats {kind, dir}` → validates as `seat add` does; 400
  `skills.seatInvalid` (not absolute / not a directory / unparseable ledger) with the reason;
  answers the new status.
- `DELETE /api/skills/seats {dir}` → forgets; 400 `skills.seatDefault` for a default seat.
- All writes run in `spawn_blocking`, as today. After any write, publish the existing plugins or
  settings notice the System page already listens to, if one exists; if none does, the page
  re-reads after its own action and that is enough (no new bus topic in this brief).

### 4.3 Panel (Settings → System → AI skills)

- One small table per seat: the seat line (kind label "Claude Code" / "Other agents", the dir in
  mono, a source hint), then one row per shipped skill with its state and per-cell
  Install / Update, as today.
- When any copy is outdated: a warn tag "N outdated" on the section and a button "Update all",
  which POSTs `/api/skills/refresh` and toasts "Updated N copies" or the failures.
- "Add Claude config directory…" opens a field sheet (`openFieldSheet`) for a path; it POSTs
  `/api/skills/seats` and shows the served reason on 400.
- A ledger seat row has a Forget action (with the house confirm pattern). A missing seat shows
  "folder missing" in a muted tone plus Forget, and no install buttons.
- Still **never installs or refreshes on mount** — the existing no-write-on-mount test must keep
  passing, and extend it to cover refresh.

### 4.4 `scripts/deploy.ps1`

After the health proof (the `/health` hash check and the `deployed:` phase line, ~line 299), run
`& $exe skill install --installed` once. Print its output. A non-zero exit is a warning line,
not a deploy failure. Respect the memory rule "never pipe deploy.ps1 output" — this is a call
inside the script, which is fine; just do not pipe the script itself in your own test.

## 5. Out of scope

- Uninstalling skills (removing a copy). Forget forgets the seat, it does not delete files.
- Discovering Claude config dirs by scanning the disk. Seats come from env and from explicit
  installs or adds only.
- Copies under `.cursor` / `.codex` (stay unread, as today).

## 6. Tests

Unit (`src/skill_install.rs` tests, tempdir `Places` plus a tempdir ledger path — add a
`ledger: PathBuf` field to `Places` or a sibling `SeatBook` type, so no test reads `data_dir()`
implicitly):

- `install_records_its_seat_once` (two installs → one ledger entry).
- `seats_union_env_and_ledger_dedupes` (same dir via env and ledger → one seat, source `both`;
  case-insensitive on Windows only, `#[cfg(windows)]` for that half).
- `installed_refreshes_every_seat_and_never_adds` (outdated in a ledger seat refreshed; absent
  stays absent; missing dir skipped and not created) — this extends today's
  `installed_refreshes_and_never_adds`, keep that test's name or rename it in the SPEC too.
- `claude_kind_renders_the_claude_text_in_a_ledger_seat` (the remote skill's `.claude`
  rendering applies to every claude seat, not just the env one).
- `unparseable_ledger_is_never_overwritten`.
- `seat_add_takes_a_config_dir_or_a_skills_dir` / `forget_refuses_a_default_seat`.

Integration (`tests/adminapi/machine.rs`): status lists seats with every path under the sandbox;
install by `dir` of an unknown seat → 400 `skills.unknownDir`; refresh route; seats add/forget
routes; the existing two tests stay green unchanged.

CLI: `swiss status` prints the outdated line only when there is something outdated.

Vitest: seat tables render; Update all POSTs refresh once; no write request on mount; Add
sheet; Forget on a missing seat.

## 7. Live proof — read this before running anything

The owner's real Claude config directory (their shell's `CLAUDE_CONFIG_DIR`) and `~/.agents` hold the skills their
agents use. **No test, and no live walk, may write them.**

- Run every CLI step with `USERPROFILE`, `HOME`, `SWISS_HOME` and `CLAUDE_CONFIG_DIR` all
  pointing into one scratch directory, and **print the seat list first** and check every dir is
  under scratch before any write. If one is not, stop.
- Do **not** use `scripts/test-instance.ps1` for this walk: it builds its own `SWISS_HOME`
  but inherits the real `USERPROFILE`, so the default `.agents` seat would be the owner's real
  `~/.agents/skills` and Update all would write it (and with a scratch `USERPROFILE` its
  snapshot step reads the wrong home). Start the `target-test` exe by hand instead:
  `serve` with `SWISS_HOME`, `SWISS_PORT=19998`, `USERPROFILE`, `HOME` and
  `CLAUDE_CONFIG_DIR` all set to scratch paths, then sign in with `swiss open` under the same
  env. Do not add `skill-seats.json` to test-instance.ps1's snapshot list either.
- Walk: install from a CLI with `CLAUDE_CONFIG_DIR=<scratch>/claude-b` → the panel on 19998 shows
  that seat; edit one copied `SKILL.md` by hand → "1 outdated" → Update all → current; delete the
  seat dir → "folder missing" → Forget. Screenshots of each state.

## 8. SPEC and docs in the same commit

- SPEC §host.cli skills paragraphs: "exactly two directories" → two **kinds**, a default seat of
  each from the env plus the seats recorded by installs and `seat add`; the ledger file; the
  refresh covers every seat; `swiss status`'s outdated line. The "no boot or panel load
  refreshes" sentence stays verbatim.
- SPEC §core.home (the data directory's file list): `skill-seats.json`, plain, not sealed.
- SPEC §panel.settings: the per-seat tables, Update all, Add, Forget.
- README: the install paragraph ("A skill goes to …") and the Update section's skill line
  ("refreshes every copy swiss has installed, in every Claude config directory it has seen").
- CHANGELOG `[Unreleased]`: Added (seats, refresh route, status line) and Fixed (the panel
  showing the wrong Claude directory when the daemon's env differs).

## 9. Delivery

Suggested commits: (1) `feat(skills): a seat ledger; installs record their seat and --installed
refreshes every one`, (2) `feat(host): skill seats on the API and the status line`,
(3) `feat(panel): per-seat skill tables with Update all`, (4) `chore(deploy): refresh installed
skills after a deploy`. Each with its tests and SPEC lines.
