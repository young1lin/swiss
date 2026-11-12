# Round 2026-11 — handoff: the order, the shared rules, the report, the review

This folder holds implementation briefs for one round of work. The briefs are written for
implementers who start with no context: another model, or a person. Each brief stands alone,
apart from this file, which every brief points to. **Read this file whole before starting any
brief.**

The briefs are not SPEC. `docs/spec/` remains the source of truth. A brief says which SPEC
paragraphs change, and they change in the same commit that makes them true.

## 1. The briefs and their order

| # | Brief | Kind | Needs |
| --- | --- | --- | --- |
| 01 | [Locale split](01-locale-split.md) | panel refactor | — |
| 02 | [Settings → About](02-settings-about.md) (version-hash, update check, diagnostics) | host + panel | 01 |
| 03 | [Skill seats](03-skill-seats.md) (every installed skill copy known and refreshable) | CLI + host + panel + deploy | 01 |
| 04 | [Self-update](04-self-update.md), in three sessions: **U1** CI assets, **U2** CLI, **U3** panel | CI + CLI + host + panel | 02 (U3), 03 (U2 refreshes skills) |
| 05 | [Process restartOnExit](05-process-restart-on-exit.md) | process crate + panel | 01 |
| 06 | [Terminal P1](06-terminal-p1.md) (shell picker, appearance prefs, OSC 52) | panel | 01 |
| 07 | [Agents Test compaction](07-agents-test-compaction.md) | llm plugin + panel | 01 |

**Order: 01 → 02 → 03 → 04-U1 → 04-U2 → 04-U3, and then 05, 06 and 07 in any order.**

- **01 goes first.** Every later brief adds locale keys into the parts that 01 creates.
- **04-U1 starts with gate E0**, a measurement. When E0 says patches are not worth it, U1
  stops, and the owner decides before U2 begins.
- **Each brief, and each of U1, U2 and U3, is reviewed (§5) before the next one starts.** The
  next brief builds on the reviewed state, never on an unreviewed one.
- **One implementer works in the tree at a time.** The tree, the CPU and the 19998 port are
  shared, so two implementers at once break each other's gates.

**Branch.** Work on one branch, `round-2026-11`, cut from `main` at the start of brief 01. Each
brief's commits go on it in order, and nothing is pushed. The owner merges.

## 2. Shared rules (every brief, no exceptions)

### 2.1 Before you start

- Read `AGENTS.md` at the repo root, then this file, then your brief, then the SPEC sections your
  brief names. Where the brief and the code disagree, the code is the fact. **Stop and report**
  the mismatch; do not improvise a different design. Where a brief says "yours" or "your
  choice", choose, and say what you chose in the report.
- Run `git status` and `git log --oneline -5`. Other agents (Cursor, Codex) sometimes work in
  this same tree.
  - Never revert, reformat, stage or commit a change you did not make.
  - If a file your brief needs has someone else's uncommitted edits, stop and report.
- Check that no `cargo`, `rustc`, `node` or `vitest` process is already running heavy work.
  If one is, wait for it to finish; never kill it.

### 2.2 Never

- **Never touch port 19999.** It is the owner's production gateway:
  - never stop, restart or redeploy it;
  - never point a test at it;
  - never run `scripts/deploy.ps1`.

  Live checks run on **19998** (§3).
- **Never write the owner's real homes:**
  - `~/.swiss` (production state);
  - `~/.agents` and the owner's Claude config directory (their shell's `CLAUDE_CONFIG_DIR`, not
    `~/.claude`): the skills the owner's agents use;
  - the repo's `bin/`.

  Briefs 03 and 04 say exactly how to stay clear of them.
- **Never print, log or commit a credential:**
  - The CLI key and session tickets stay unprinted: use `swiss api` (it authenticates itself).
  - Config holds references only: `${ENV_VAR}` or `${secret://name}`.
  - Never commit `gateway.config.json`, `.env*`, `managed.json`, `tunnels.json`, `master.key`,
    `session.json`, `secrets.json`, `env.json`, `*.log` or `*.cast`.
  - No file may contain an email address or an absolute personal path; the repo is `<repo>`,
    and a home is `~`.
- **Never run a local model.** The machine is low on RAM. A live LLM check uses the GLM provider
  from the repo-root `.env` (gitignored), and never the local ollama models. The 19998 home is a
  snapshot of production and inherits production's local embedding model, so a live memory
  save, search or ask there would load it. Turn the test home's embedding off first, or skip
  memory calls entirely.
- **Never run `cargo fmt`** on the tree. Format only what you write, by hand, to the file's
  existing style.
- **Never push, tag, or bump the version.**

### 2.3 How to build and test on this machine

The machine has a small paging file. Saturating the CPU makes tests fail at exactly their own
deadlines, which is noise, not bugs.

- **One heavy job at a time.** Never run `cargo` beside `vitest` or `npm run check`, and never
  two `cargo`s at once.
- **cargo:** always `-j 2`, and test binaries with `-- --test-threads=1`. Run only the tests your
  change touches (`cargo test -j 2 -p <crate> -- --test-threads=1 <filter>`); there is **no full
  gate** here. Write output to a file outside the repo and read it back. Do not re-run a full
  suite to chase a flake.
- **vitest:** `npx vitest run --maxWorkers=2 <files>` for the touched test files, then
  `npm run check` **once, alone**, in `crates/swiss-panel/panel` at the end of a panel brief.
- **A test that fails at exactly its own timeout while other work ran is CPU starvation.**
  Diagnose it from the captured output, and allow one re-run on an idle machine at most.
- **Panel emit:** after any panel `.ts` or locale change, run `npm run build` in
  `crates/swiss-panel/panel` and commit the regenerated `crates/swiss-panel/src/admin_assets/js/**`
  files with the source. `build:check` fails STALE otherwise.
- **A release build** for the 19998 instance:
  `CARGO_TARGET_DIR=target-test cargo build --release -j 2`, alone. It takes minutes, and its
  memory peaks around 2 GB. For a quick compile check, use `cargo build -j 2 --profile quick`.
- **Antivirus.** If a build output (`swiss.exe`) vanishes, or "could not be overwritten", the
  antivirus took it. Stop and report it. Do not rename, relocate or work around it.

### 2.4 How to write the code

- **Code comments in English**, always: inline, block and doc comments, `TODO` text, and
  comments inside code samples in docs.
- **Panel strings:** every user-visible string goes through `tr()`/`trn()`, with the key added to
  **both** `en` and `zh` in the right part (after 01: `locales/en/<part>.ts` and
  `locales/zh/<part>.ts`). A refusal the server sends uses `admin_error_code(status, code, msg)`,
  and the panel maps `code` through `SERVED_CODES` to a `served.*` key.
- **File size:** a file past about 4 000 lines is too big. Do not grow one; put new code in a new
  file or module, as your brief says (several briefs name the file).
- **Fixtures and examples** use neutral names (`acme_app_*`, `jdoe`, a target called `dev`).
  Never use real hostnames, databases or users (§release.hygiene).
- **Edit tooling:** if your editor tool does regex-style replacement, a `$` in the replacement
  text (`$1`, `$&`, `` $` ``) may be expanded. Check the result.

### 2.5 Commits

- One commit per work item, with the messages your brief suggests (Conventional Commits, as in
  `git log`).
- Each commit carries its own tests, strings, SPEC lines, CHANGELOG line and panel emit, and
  stands on its own.
- **No `Co-Authored-By` trailer, of any capitalisation, and no other attribution lines**
  (§release.hygiene: the only author is `young1lin`). If your tool adds one by default, remove
  it.
- Name, in each commit message body, the tests you ran, and say that the full gate did not run.
- Screenshots of a live instance are **never committed**. They go in the report, saved outside
  the repo.

## 3. The 19998 test instance

- **The usual way:** build `target-test` (§2.3), then `scripts/test-instance.ps1`.
  - It snapshots production's state into `%LOCALAPPDATA%\swiss-test-home` and serves on 19998.
  - `-Fresh` wipes that home first, and `-Stop` kills whatever owns 19998 (by port, never by
    name).
- **Briefs 03 and 04 must not use the script** for their walks. It inherits the real
  `USERPROFILE`, and their walks write skill seats. Their briefs say how to start the exe by hand
  under scratch env.
- **The snapshot holds real things:**
  - The owner's database connections: run read-only statements only.
  - The owner's process apps (dsh among them): never start one, or it fights production for its
    ports.
  - Production's local embedding model (§2.2).
- **A browser without an extension.** Sign in with a one-time ticket. Run
  `target-test\release\swiss.exe api POST /api/session/ticket {}` with
  `SWISS_HOME=%LOCALAPPDATA%\swiss-test-home` and `SWISS_PORT=19998`, then open the returned
  `url`.
  - From Git Bash, prefix `MSYS_NO_PATHCONV=1`.
  - The ticket works once. Never print it into the report.
  - Headless Chrome or Edge driven through CDP works for screenshots.
  - Pages are hash routes (`#system`, `#llm-agents`, `#data`, …).
- **Panel-only changes without a rebuild:** a release build embeds the assets. To check new panel
  JS without rebuilding, either rebuild `target-test`, or intercept `*/admin/js/*` in the
  headless browser and serve the files from `crates/swiss-panel/src/admin_assets/js` after
  `npm run build`. Say which you did.
- When you are done, stop 19998 (`scripts/test-instance.ps1 -Stop`, or kill your own hand-started
  pid).

## 4. The delivery report (reply with exactly this)

```
## Brief NN (or 04-U1/U2/U3): <title>

### Commits
<hash> <subject>
...

### What was built
- one line per work item, naming the files

### Tests run
- the exact commands, and the tail of each output (pass/fail counts); say "full gate not run"
- for a refactor: the before and after outputs of the pinned tests

### Live proof
- the steps as run on 19998, what was seen, and the screenshot paths (outside the repo)
- pasted output where the brief asks for it (masked where anything secret-looking appears)

### SPEC and docs
- each SPEC section changed, plus README / CHANGELOG / SETUP lines

### Deviations from the brief
- what differs, and why; or "none"

### Open questions
- anything the reviewer or the owner must decide; or "none"
```

If you stopped early (a gate such as E0, a mismatch, a foreign edit, the antivirus), send the
same report with what was done, and the reason under *Open questions*.

## 5. Review (what the reviewer does with each report)

The reviewer reads the diff (`git diff <base>..<tip>`), not only the report, and checks:

1. **The brief.** Every work item is present and behaves as written. Every *Deviation* has a
   reason that holds. Nothing outside the brief changed (no drive-by refactors, no formatting
   churn).
2. **The facts the brief rested on still hold** in the changed code: the seams, the unset-seam
   503, the busy checks, `version_line()` unchanged, and the like.
3. **Safety.** No write to 19999, `~/.swiss`, `~/.agents`, the owner's Claude config directory or `bin/`, in
   tests or in code paths that tests reach. No credential in code, tests, fixtures or output. Each
   brief's own *Risks* list (04 §8, for example) is checked item by item.
4. **Tests.** The reviewer re-runs the brief's targeted tests (serially, capped) and reads the
   new tests for real assertions. A test that would pass with the feature removed does not count.
5. **SPEC.** The paragraphs the brief names were amended, and they read true against the code.
   No SPEC line now describes something that is not built.
6. **Panel.** Strings exist in both `en` and `zh`, in the right part. The emit is committed and
   not stale. Keyboard and accessible names are as the brief asks. Nothing is fetched on mount
   that the brief says happens only on click.
7. **Hygiene.** No `Co-Authored-By`, no email address, no absolute personal path, no committed
   screenshot or state file. Comments are in English.

**Outcome**, one of:

- **Accept**: the next brief may start.
- **Fix list**: numbered findings, each with file:line and the expected behaviour. They go back
  to the same implementer, who fixes them in new commits (no history rewrite) and reports again
  in the same format.
- **Stop**: the design itself needs the owner's decision; the reviewer says what to decide.
