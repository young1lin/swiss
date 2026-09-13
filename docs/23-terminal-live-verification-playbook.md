# 23 · Terminal panel live verification playbook

The steps that verified the P0/P1 terminal work (docs/22) on a real gateway and a real
browser. Every rule here was paid for once: most entries record a trap that produced a
wrong conclusion or a silently broken verification during the 2026-09-12/13 sessions
(Node commits e3d7d9a..a750b20, worktree b4c1900..6fda158).

Scope: the terminal page specifically. The generic loop (state snapshot, test home,
health probe) lives in the `swiss-live-verify` skill and AGENTS.md "Live testing ports";
this doc owns what those do not cover — the worktree port variant, the byte-mirror
pipeline, the browser-side test matrix, and the automation traps.

## 1 · Ports and the worktree instance

| Port | Role |
|------|------|
| 19999 | the human's production instance — never stopped, never redeployed, never sent iteration traffic |
| 19998 | the repo-standard test instance (scripts/test-instance.ps1); other plans held it during these sessions |
| **19996** | the terminal-parity worktree instance this playbook uses |

The worktree must never touch the main checkout's `target` or home, so the instance gets
its own everything (all three are env vars — `start`/`serve` would otherwise write the
port into config; never pass `--port`):

```powershell
# run from the worktree root: .agents/worktrees/terminal-parity
$TestHome = Join-Path $env:LOCALAPPDATA 'swiss-test-home-wt'
$env:SWISS_HOME  = $TestHome        # sealed-state snapshot incl. the DPAPI-copied master.key
$env:SWISS_PORT  = '19996'
$env:CARGO_TARGET_DIR = 'target-test'   # the production daemon holds target/release/swiss.exe
```

Lifecycle rules, each learned the hard way:

- **Kill by the port's owning PID, never by process name** — `Get-Process swiss` kills
  production too. Before a rebuild this is not optional: a running 19996 keeps
  `target-test\release\swiss.exe` locked and cargo dies with `os error 5` **after two
  minutes of linking**, looking like a build failure.
- **Always re-check `/health` after a restart and read the build hash.** A restart whose
  Start-Process silently failed leaves the old process serving, and everything after it
  verifies the wrong binary. The hash must name the commit you think you shipped.
- The junction `.agents/worktrees/local-mcp-gateway` → the Node repo makes the
  byte-for-byte guard genuinely run from inside the worktree. It is gitignored; create it
  when entering, remove it when leaving.

## 2 · The iteration pipeline (one full lap)

Panel JS is authored in the Node repo and copied in — editing `admin_assets` directly is
never valid (AGENTS.md "The panel's JavaScript is the spec"). One lap:

```text
1. edit        <node-repo>\src\admin\...   (the ONLY edit site)
2. unit tests  npx vitest run test/admin-terminal.test.ts test/admin-panel.test.ts
               test/admin-terminal-vendor.test.ts
3. parse check node vm.SourceTextModule on views/terminal.js (syntax-only; the view
               cannot be imported — connect.js touches document at top level)
4. mirror      robocopy <node-repo>\src\admin crates\swiss-panel\src\admin_assets /MIR
5. byte guard  cargo test -p swiss-panel   (the_tree_is_byte_for_byte_the_node_builds)
6. gates       cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
7. build       $env:CARGO_TARGET_DIR='target-test'; cargo build --release   (~2-3 min)
8. restart     kill 19996 by port PID, start with the env vars above, verify /health hash
9. live verify the matrix in §3, through a real browser
10. commit     Node repo commit, then a worktree commit containing ONLY admin_assets
               (docs commits stay separate; sibling panel work swept in by the mirror
               gets named in the commit message, never hidden)
```

The guard is the honest step: it re-hashes every file in both trees. When it fails with
no new commits on either side, the sibling tree has uncommitted edits (another session
works in that repo concurrently) — re-mirror and say so in the commit message.

## 3 · The browser verification matrix

Automation: chrome-devtools MCP. Every call needs `pageId`; `evaluate_script` takes a
`function` string (`function() {...}` — arrows parse-fail); `fill` uses `value`.

Open `http://127.0.0.1:19996/#terminal`, open a session, and **wait for the prompt
before typing**: Store pwsh cold-starts slowly, and keystrokes sent before the shell is
up race its initialization (the pre-launch input queue covers the pre-WebSocket gap, not
a slow PTY child). Then:

| # | Feature | Steps | Pass |
|---|---------|-------|------|
| 1 | Title sync | \`Write-Host "\`e]0;TITLE\`a"\` + Enter | tab label = TITLE |
| 2 | Rename, right-click | synthetic `contextmenu` on the tab → type → Enter | input appears prefilled; label changes |
| 3 | Rename precedence | after #2, emit OSC 0 with a different title | label unchanged (manual name wins) |
| 4 | Rename, double-click | two REAL clicks on the tab (see trap T1) + one node-survival check | rename input appears |
| 5 | Bell | \`Write-Host "\`a"\` | `.term-tab-bell` dot in the tab |
| 6 | Find bar, BODY focus | Ctrl+Shift+F with focus on BODY (not the terminal) | bar visible, `#term-find-q` focused |
| 6b | Find bar, TERMINAL focus | focus the xterm textarea first, then Ctrl+Shift+F | bar STAYS open — a flicker-open-shut is the double-fire regression (xterm consults its custom key handler without checking defaultPrevented, so the page-level listener must yield the whole .term-holder subtree) |
| 7 | Find live count | type a string that exists twice | counter `1/2`-style, updates per keystroke debounce |
| 7b | Find does not copy | with copy-on-select on, navigate matches | NO scissors pill — the addon selects each match it moves to; an unguarded onSelectionChange would overwrite the clipboard per keystroke |
| 8 | Find close | Esc | bar hidden, decorations cleared |
| 9 | Pin + chip | see §4 — the delayed-output pattern | view holds line N, chip "N new ↓", click → bottom + chip hidden |
| 10 | Multiline paste | clipboard 2 lines + Ctrl+V | native confirm; Enter accepts; both lines execute (find proves output) |
| 11 | Alt tab jump | 2 sessions, Alt+1 / Alt+2 from terminal focus | active tab switches |
| 12 | Tab-bar repaint | open rename → Esc; then navigate #jobs → #terminal (a settings save hits the same reload path) | input gone after Esc and rename reopens; the bar is PAINTED after remount — an empty bar is the memo-skip regression |
| 13 | Console sweep | `list_console_messages` at the end | no errors (an a11y notice about a missing id is a fix-me, not a pass-blocker) |

## 4 · The pin/chip test — the one that needs choreography

Typing into the terminal re-pins the view on purpose (every keystroke is
`toBottom()` — VS Code semantics). So output generated by typing can never demonstrate
the chip. The pattern:

```powershell
# 1. ensure real scrollback first (a full-height viewport may hold 100+ rows)
Write-Host ((1..300 | ForEach-Object { "filler $_" }) -join "`n")
# 2. one command whose output lands LATER, typed while at the bottom
Start-Sleep 4; Write-Host ((1..25 | ForEach-Object { "late $_" }) -join "`n")
# 3. during the sleep: wheel up on the terminal
# 4. the output lands while the reader is scrolled away
```

Pass: `scrollTop` stays at the pre-burst value through the 25 lines (writeTerm's
capture-and-restore holds it), `.term-jump` shows `hidden=false` with `25 new ↓`,
clicking it returns `scrollTop` to max and hides the chip.

## 5 · Automation traps (each produced a wrong conclusion once)

- **T1 — synthetic dblclick proves nothing about dblclick.** The rename bug (the tab bar
  rebuilt between the two clicks of a double-click, so dblclick never fired) was
  invisible to a dispatched `dblclick` event — it bypasses click sequencing entirely.
  And two real CDP clicks usually exceed the browser's ~500 ms double-click window.
  Verify the precondition instead: the button node survives one real click
  (`data-marker` tag), plus the right-click path synthetically.
- **Wheel events must dispatch on `.xterm-viewport`**, never on the holder — the
  holder's listener never sees events aimed at a descendant.
- **Target the visible terminal explicitly**: `.term-holder:not([hidden])`. A page that
  adopted old sessions carries several holders, and a bare `.term-holder` selector hands
  you a hidden one with zero size (`scrollHeight 0`) — every assertion on it is vacuous.
- **A memo that skips repaints must reset wherever the DOM changed outside it.** The
  identical-markup skip stranded a rename input forever on Escape and left a fresh
  remount's tab bar empty — both shipped green through automation because the paths
  (settings save, page remount, Escape-commit) were never walked. When a cache guards
  DOM writes, audit every writer that bypasses it.
- **A native `confirm()` blocks the renderer.** Symptom: `evaluate_script` and even
  `handle_dialog` time out. Answer it with a raw key dispatch (`press_key` Enter /
  Escape) — keys reach the dialog layer even while JS is parked.
- **Keyboard focus decides what a shortcut tests.** "Ctrl+Shift+F does not work" from a
  human was focus-scope: bound inside xterm, dead when focus sat elsewhere. Test the
  shortcut twice — focus in the terminal and focus on BODY.
- **Enough content or no scrollback**: on a tall full-pane viewport a 60-line burst does
  not overflow. `scrollHeight == clientHeight` means the wheel could never unpin; check
  the range before blaming the pin logic.
- **/health hash after every restart** (see §1) — the wrong-binary failure mode is silent.

## 6 · Teardown

- The 19996 instance is disposable: kill by the port's owning PID when done, or leave it
  up for the human to poke at and say so.
- The test home (`swiss-test-home-wt`) is a snapshot; a save there never touches the
  user's config. Recordings under its \`terminal/*.cast\` are still output-only secrets
  risk — do not commit them anywhere.
- Deploying to 19999 stays a separate, explicit, last step (`scripts/deploy.ps1` owns the
  whole order); nothing in this playbook touches it.
