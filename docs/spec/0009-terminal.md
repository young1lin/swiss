# §terminal — Terminal

Chapter 0009 of the [swiss specification](../SPEC.md).

A browser terminal on the panel: a **local shell** (ConPTY on Windows, openpty on unix) and
**remote shells** on the SSH connections the tunnels plugin already holds. It is a developer's
desk, not a bastion host: one user on loopback, a handful of sessions, no accounts. Multi-tenant
access, RBAC, credential vaults and approval workflows are what bastion products are for; none
of them belong here. The frontend is xterm.js, the one mature browser terminal (VS Code's
integrated terminal and every well-known web terminal use it).

**Where it lives.** The crate split keeps SSH and HTTP out of the session machine (§arch.crates):

| Piece | Location |
|---|---|
| Session machine: config, sessions, tickets, recording, local shells | `crates/swiss-terminal/src/terminal/` — no axum, no russh |
| Routes and the WebSocket pump | `crates/swiss-plugins/src/terminal_api.rs` (inside the loopback guard and the plugin boundary) |
| Descriptor, config schema, validation | `crates/swiss-plugins/src/terminal.rs` |
| The PTY seam | `crates/swiss-core/src/platform/pty/` (`conpty.rs`, `openpty.rs`) |
| The shell capability seat | `crates/swiss-host/src/services/shell.rs` |
| The SSH shell provider | `crates/swiss-tunnels/src/tunnel/shell.rs` |
| Panel | `panel/src/views/terminal.ts`, `terminal-core.ts` (pure, unit-tested), `views/terminal-settings.ts`, `term-overlay.ts` |

**Descriptor.** id `terminal`, version `0.1`, config schema version 1, one page (order 70,
`sidebar: false` — the MCP sidebar is the MCP page's chrome), routes `["/api/terminal"]`,
`restart_on_config_change: true` (saving the config restarts the plugin and closes every
session), and `requires: []`. It does **not** require the SSH shell capability: a local session
needs no SSH, so the remote half reports the provider's presence instead of gating the plugin
on it (§terminal.remote).

**Out of scope, deliberately:**

- **A command blocklist.** Filtering commands on an interactive PTY byte stream is security
  theater — `e""cho`, aliases, pastes, TUI programs, `base64 -d | sh` all walk past it. Restrict on
  the remote host with real mechanisms (sudoers, a restricted shell).
- Multi-tenancy, RBAC, a credential vault, ticketed approval (see above).
- SFTP or a file manager — a different plugin with its own section, if ever.
- Session sharing or co-viewing: in a single-user loopback process its only use is a demo.
- Widening the panel's asset types (fonts, wasm) — §terminal.panel.
- New `PageDescriptor` fields or a different `/api/plugins` shape.

## §terminal.api — Routes, tickets and the wire

| Route | Does |
|---|---|
| `GET /api/terminal/targets` | What can be opened: `local {enabled, shell, shells}` and `remote {presence, reason?, targets}` |
| `GET /api/terminal/sessions` | The live sessions: `id, target, label, opened, bytesOut, attached, recording` |
| `POST /api/terminal/sessions` | Open `{target, cols, rows, shell?}` → 201 `{id, ticket, recording}` |
| `POST /api/terminal/sessions/{id}/ticket` | Mint a fresh one-time ticket for a reconnect → `{ticket}` |
| `GET /api/terminal/sessions/{id}/stream?ticket=` | The WebSocket |
| `POST /api/terminal/sessions/{id}/resize` | `{cols, rows}` → `{id, resized: true}` |
| `DELETE /api/terminal/sessions/{id}` | Close → `{id, closed: true}` |
| `GET /api/terminal/recordings` | The stored recordings, newest first (`uid, sid, started_ms, cols, rows, ended_ms, end_reason, capped, imported, bytes`): the discovery surface for a closed session's export URL, which the live list drops at close |
| `GET /api/terminal/recordings/{sid}/cast` | One stored recording as a byte-faithful asciicast v2 export (§terminal.recording); 404 when the sid has no recording or the store is not open, 409 when the export guard refuses a recording past 64 MB |

- **Strict bodies.** The open body takes only `target`, `cols`, `rows` and `shell`; an unknown
  field is a 400 naming the known ones. `target` is a non-empty string (`local` or a connection
  id). `shell` is an optional one-session override of the local program.
- **Geometry** is `1..=1000` on both axes, checked as a `u64` before the narrowing cast (70000 as
  `u16` would wrap into range). Out of range is a 400, never clamped: a 0-column PTY is undefined
  behaviour on the far side. `PtySize::new` in the shell seat is the single authority.
- **Errors.** `NoSession` 404, `Ticket` 403, `Refused` 409 (a cap, a disabled local shell, a
  target not in `allowedTargets`), `Shell` 502 (the provider failed). The machine's message
  passes through verbatim. Every route answers a structured 503 while the plugin is not running.
- **Targets view.** `local.shell` is the **resolved absolute path** of the program a session
  would get, and `local.shells` is `[{program, label}]`, the candidates probed at plugin start
  (§terminal.local). `remote.presence` is `serving`, `stopping` or `absent`; `reason` names the
  provider plugin whenever the list is empty for a reason other than "you have no connections",
  because an empty list alone reads as the wrong story. Each remote target carries
  `id, label, host, port, username, state`. The `local.enabled`/`local.shell` fields keep their
  shape for older panels.

**Tickets.** The loopback guard (§security) checks peer, `Host` and `Origin`, and a browser's
WebSocket handshake carries `Origin`, so the guard already holds for the stream. The ticket makes
"can open a terminal" explicit, so that loosening the Origin check some day cannot loosen the
terminal with it. A ticket is 24 hex characters (96 bits from the OS CSPRNG), lives 10 s, is
single use and bound to one session id. Presenting it for the wrong session burns it; closing a
session forgets its tickets. The ticket minted at open cannot serve a reconnect (10 s against a
60 s grace window), so the panel mints a fresh one for every connect. The upgrade extractor is
checked **before** the ticket is spent — a handshake that cannot upgrade must not cost the ticket
— and the ticket verdicts are HTTP statuses decided before the 101, not a socket that opens and
dies. A missing ticket is a 400 naming the mint route.

**Frames.** Binary frames are raw PTY bytes in both directions — no base64, no JSON wrapping.
Text frames are control objects: `{"t":"resize","cols","rows"}` from the client;
`{"t":"exit","code"}`, `{"t":"error","message"}` and `{"t":"stalled"}` from the server. Data, exit
and error share one ordered channel, so an exit never overtakes the output before it. An unknown
control object is ignored, never fatal. Liveness is WebSocket ping/pong. Keystrokes have their own
write side: a parked outbound send never blocks input, so Ctrl-C reaches a program that is
flooding the terminal.

**Resize has two channels on purpose.** The `resize` frame while the socket is up, `POST
/resize` while it is not (a reconnect gap). Both land on the same machine call.

**Transport.** WebSocket through axum's `ws` feature; the net new crates are `tokio-tungstenite`
and `tungstenite` (the rest of the feature — hyper, sha1, base64 — was already in the graph).
SSE plus a POST input channel was rejected: it trades one crate for three bug classes (input
ordering, half-open liveness, an output stream that died while input lives on).

## §terminal.sessions — The session machine

- **One driver per session** owns the PTY or channel and takes commands over a bounded queue
  (32). The HTTP layer never touches a PTY directly.
- **Fan-out attach.** Any number of sockets can attach to one session and all receive its output
  — a second panel tab must not blind the first. The last detach starts the grace clock.
- **Grace and catch-up.** A dropped socket is not a dead session: for `graceSeconds` (60) the
  output goes into a catch-up buffer and a reconnect replays it. A closed laptop lid must not kill
  a running compile. The buffer is **64 KB per session, a constant, not config** — it is a line in
  the memory budget (§terminal.budget). It drops from the front and tells the reattaching client
  so: `[gateway] N bytes of output were dropped while this terminal …`.
- **Backpressure, never loss.** Each attached client has a 16-frame queue. When it is full the
  driver **stops reading** the PTY or SSH channel; the pipe and the SSH window carry the pressure
  back to the program, which is how a real tty behaves. Bytes are never dropped: one lost fragment
  of an escape sequence corrupts the display for the rest of the session. After 1 s of a full
  queue the client gets `{"t":"stalled"}`; a queue full for `stallSeconds` (30) is a client that
  is gone, and the session closes with that reason.
- **Four clocks:** idle (`idleTimeoutMinutes`, 30; output counts as activity; 0 = never), grace,
  stall, and the recording cap (§terminal.recording).
- **Caps.** `maxSessions` (4) and `maxSessionsPerTarget` (2) govern **remote targets only**. Local
  sessions are uncapped and do not spend the remote budget (owner decision 2026-09-12). A
  reservation holds the slot from the cap check to the open, so two concurrent opens cannot both
  pass the last free slot.
- **Every close says why.** The sentence goes into the terminal as a visible `[gateway]` line
  (except when the shell exited on its own — it already said goodbye), into the recording's end
  marker and into the `error` frame. Final frames get 250 ms to flush.

| Close reason | Sentence |
|---|---|
| Exited | `the shell exited` / `the shell exited with code N` |
| Failed | the provider's own message |
| Idle | `closed after N minutes with no activity (idleTimeoutMinutes)` |
| Stalled | `closed: this terminal accepted no output for N seconds (stallSeconds)` |
| Abandoned | `closed: nothing reconnected within N seconds of the socket dropping (graceSeconds)` |
| Requested | `closed on request` (DELETE, or the panel closing the tab) |
| Shutdown | `closed: the terminal plugin is stopping` |

Stopping the plugin closes every session with the Shutdown reason; drivers, PTYs and recordings
follow the sessions down (§host.lifecycle).

## §terminal.config — Config

`plugins.terminal.config`, parsed strictly (`TerminalConfig::parse`): an absent key takes its
default, a present key of the wrong shape is a user-facing 400 through `validate_config` naming
the key. A mistyped limit that silently fell back to the default would be a limit the user
believes in and does not have.

| Key | Default | Rule |
|---|---|---|
| `local.enabled` | `false` | The local shell is off until the user turns it on |
| `local.shell` | `""` | Empty = the platform default (§terminal.local) |
| `allowedTargets` | `[]` | Exact connection ids; empty = every connection the provider lists |
| `maxSessions` | 4 | 1..=64; remote only. Zero is refused — a plugin that can open nothing is a disabled plugin |
| `maxSessionsPerTarget` | 2 | 1..=64, and never above `maxSessions` |
| `idleTimeoutMinutes` | 30 | 0..=86400; 0 = no clock |
| `graceSeconds` | 60 | 0..=86400 |
| `stallSeconds` | 30 | 0..=86400 |
| `recording` | `true` | Off is honest: the list and the open response report `recording: null` |

**Why the local shell is off by default.** The gateway already runs as the user, so a local shell
gives a local attacker nothing new — but it turns a loopback HTTP port into arbitrary code
execution, and that upgrade is worth one deliberate click. The switch lives on the terminal page
(§terminal.panel). The terminal never holds a credential: remote shells arrive as byte streams
from the provider, and anything secret in config follows §host.refs.

## §terminal.recording — Recording

- **asciicast v2 shape, stored in the terminal history** — `logs/terminal.db` through the host's
  module-history store (ADR-035): a header object (`env.TERM = "xterm-256color"`) plus one
  `[elapsed, "o", text]` line per chunk. The event lines are stored VERBATIM as sealed body
  parts, so timing survives storage and a `.cast` file is an export the gateway reconstructs on
  demand (`GET /api/terminal/recordings/{sid}/cast`), not a file it owes anyone. Elapsed
  time is monotonic. UTF-8 is reassembled across chunk boundaries (at most 3 bytes held back).
- **First open imports the legacy tree** (ADR-033's contract, verbatim): every existing
  `~/.swiss/terminal/<sessionId>.cast` folds in idempotently (natural key = session id, torn
  tails counted in meta, never a crash), and the source files are LEFT IN PLACE — retiring
  them is the operator's explicit call after the cutover is verified.
- **Output only.** That is what asciicast is — and it does **not** mean passwords stay out: a
  shell echoes what is typed, and an echoed secret is in the output stream. Only unechoed input
  (a sudo prompt) is truly absent. Treat every recording as sensitive: sealed parts never
  leave the machine unencrypted, the export route sits behind the admin session, and
  recordings never go into a commit, an issue or a report.
- **8 MB cap, no rotation.** At the cap recording stops with a visible marker (`[gateway]
  recording stopped at the 8 MB cap; the session is still running …`); a session end writes
  `[gateway] session ended: <reason>`. Silent rotation was rejected: a recording that quietly lost
  its beginning is worse than one that says where it stopped.
- A recording failure warns and never takes the session down; `recording: false` in config
  turns it off for new sessions. The session list's `recording` field is the export URL of the
  stored recording, null when the session is not recorded.
- **A plugin stop lands the tail, bounded.** Each driver flips its closed flag only AFTER its
  final part and finish stamp are submitted, and the goodbye delivery to (possibly stalled)
  sockets happens after the flag, spending one shared CLOSE_FLUSH budget per frame — so the
  stop's per-driver wait (750 ms) can never race an unbounded goodbye fan-out, and a stalled
  reader cannot stall the store's close behind it.
- **Retention: the 90-day sweep.** Recordings join the hourly maintenance sweep at ninety
  days — a recording's evidential value decays faster than a call log's (MCP keeps 180 days),
  and the 8 MB per-session cap already bounds the pathological case. An imported legacy
  recording is ENDED at its own recorded timestamp, so it joins the sweep on the age it
  actually has, not on the day the import folded it in — the legacy pile ages out from its
  own clock. The sweep also collects stored parts whose session row never landed (a refused
  begin, a lost race): a part no index row can name is disk debt, not history. The terminal's quota
  share is 100,000,000 bytes (§host.history); an over-share denies new recordings with the
  same honest `over-owner-share` refusal as every other owner.
- **Crash recovery preserves live owners.** Open and maintenance adopt an unfinished recording
  only when its owner is absent, its known creation time proves PID reuse, or it predates owner
  tracking. An unavailable creation time (including macOS/BSD) is unknown, not proof of death;
  a live owner with incomplete identity stays live and outside the age sweep.

## §terminal.local — Local shells

- **The PTY seam** is `swiss_core::platform::pty`: ConPTY on Windows, `openpty` on unix. The ConPTY
  FFI is written here rather than taken from `portable-pty` (ADR-011: the seam is small, and the
  crate would bring its own process and thread model).
- **One blocking read thread per session** (`spawn_blocking`, 32 KB reads, a depth-1 channel):
  a ConPTY's output is an anonymous pipe, and parking on the full channel **is** the backpressure.
- **Environment:** `TERM=xterm-256color`, `COLORTERM=truecolor`, `NO_COLOR` removed.
- **The program, in precedence order:** the open request's `shell`, then `local.shell`, then the
  platform default. The default on Windows is `pwsh.exe` if PATH resolves it, then
  `powershell.exe`, then `COMSPEC`, then `cmd.exe`; on unix `$SHELL`, then `/bin/sh`.
- **PATH lookup** (`find_on_path`, a pure function taking PATH as an argument so tests never touch
  the process environment): the name is tried as given first; `PATHEXT` is appended (lowercased)
  only when the name has no extension; a hit is `metadata().is_file()`. No `canonicalize` — the
  Store build of pwsh is an execution alias under `WindowsApps`, and canonicalizing resolves it
  into a versioned package path that is ugly and breaks on the next update.
- **Candidates** for the settings sheet — PowerShell 7, Windows PowerShell, cmd, plus
  `%ProgramFiles%\PowerShell\7\pwsh.exe` and `%ProgramFiles%\Git\bin\bash.exe` — are probed once
  at plugin start and cached, never per request, and never from the registry. Labels are
  `PowerShell 7`, `Windows PowerShell`, `cmd`, `Git Bash`, or the file's basename.
- **Process tree.** The child joins a kill-on-close job object (`KillOnCloseJob`, ADR-008); unix
  kills the process group. Killing `swiss` leaves no orphan shell (§process).

## §terminal.remote — Remote shells

- **The seat.** `ShellRegistry` in `RuntimeServices` (`swiss-host/src/services/shell.rs`) holds at
  most one provider. A second registration is refused naming the incumbent. A provider that
  withdraws stops new sessions at once and gives the live ones a 3 s drain. `PtySession` is
  transport-neutral (bounded input and output queues of 32 chunks); ledger leases are scoped to
  the session. This seat is why the terminal crate never depends on the tunnels crate
  (§arch.crates, ADR-011).
- **The provider** is the tunnels plugin (`TunnelShells`, provider id `tunnels`): a target is an
  existing tunnels connection, and `TunnelManager::open_shell` opens a PTY channel on it — no second
  credential, no second host-key decision (§tunnels.ssh). Waiting for the `want_reply` receipt of
  the PTY and shell requests must skip `WindowAdjusted` messages. The connection guard moves into
  the pump, so the connection stays up exactly as long as the shell.
- **Presence.** With the tunnels plugin disabled the remote list is empty **and the reason names
  the tunnels plugin**; while it stops, the reason says no new remote sessions start. Local
  sessions are unaffected either way.
- `allowedTargets` narrows the list; a target outside it is refused with a 409.

## §terminal.panel — The terminal page

**Vendoring xterm.** xterm 5.5.0 with addon-fit 0.10.0, addon-unicode11 0.8.0, addon-web-links
0.11.0, addon-webgl 0.18.0 and addon-search 0.16.0 (lazy-loaded on first use), under
`vendor/xterm/<package>-<version>/` (§panel.vendor):

- The npm **dist** UMD files, byte-identical — no minification, no patches; wrap in our own code
  instead. Each directory carries an `index.js` shim naming the package and exact version, loaded
  through `load-classic.js`. Never build from the xterm.js source tree (it is 6.0-dev, with
  incompatible internals).
- An upgrade is a new directory plus the import change, and the old directory is deleted in the
  same commit, so `git log` answers which xterm shipped.
- The asset pipeline serves only UTF-8 `.html`, `.css`, `.js` and `.svg` (`mime_of`), which is part
  of why the tree can be compared byte for byte. So: the system monospace stack
  (`ui-monospace, SFMono-Regular, Consolas, "Cascadia Mono", monospace`), no web or Nerd fonts, and
  no wasm. Rendering is WebGL with a DOM fallback on context loss (there is no canvas addon in 5.5).
  After a font zoom the WebGL texture atlas is cleared.
- Facts verified against 5.5 (do not build on the opposite): no `bellStyle`/`bellSound` options (the
  beep is generated); `getOption`/`setOption` are gone (use live `term.options`); load Unicode11
  before setting `activeVersion`; search decorations need `allowProposedApi: true`. Addons checked
  compatible with the vendored core: search 0.16.0, clipboard 0.2.0, serialize 0.14.0, progress 0.2.0.

**Keys (Windows Terminal semantics, not xterm's Linux defaults).** Pure logic in
`terminal-core.ts` (`keyAction`, `mouseAction`), pinned by `test/admin-terminal.test.ts`:

| Input | No selection | With a selection |
|---|---|---|
| Ctrl+V, Ctrl+Shift+V, Shift+Insert | paste | paste |
| Ctrl+C | `^C` — SIGINT, never changed | copy and clear the selection; no `^C` |
| Ctrl+Shift+C, Ctrl+Insert | nothing | copy |
| Right-click | paste the clipboard | copy and clear the selection |
| Shift+right-click | the browser's menu (the escape hatch) | same |
| Ctrl+= / Ctrl+- / Ctrl+0, Ctrl+wheel | zoom 8–32 px (default 13), remembered per browser | |
| Ctrl+Shift+F | find bar | |
| Alt+1..9, Alt+←/→, Alt+W | jump to tab, previous/next, close (the browser keeps Ctrl+Shift+W and Ctrl+Tab) | |

- Keyboard paste returns `false` from the custom key handler **without** `preventDefault`, so the
  browser's native `paste` event reaches xterm's textarea — no clipboard permission prompt. Right-
  click paste uses `navigator.clipboard.readText()` and `term.paste()` (bracketed paste); a refusal
  toasts "use Ctrl+V". Copy is `writeText` then `clearSelection`; a failure toasts, never silent.
  Focus returns to the terminal after every action.
- Shift+Tab is `preventDefault` but returns `true` (the key must reach the terminal, or browser
  focus escapes). Alt+F4 passes through.
- The paste payload is never rewritten (no `\n → \r`): bracketed paste is what bash and PSReadLine
  expect. A paste of more than one line asks first (a native confirm previewing the first 1000
  characters) — except on the alternate screen, where multi-line is normal.

**Tabs and sessions.** Every session is a tab; a page mount adopts the sessions already open; a
closed session keeps its tab so the scrollback stays readable. The label precedence is a manual
name, then the shell's OSC 0/2 title, then the connection name (`tabLabel`); an empty title falls
back, and shells that never send one do not stall anything. Rename by double-click or right-click
on the tab; middle-click closes. A mount epoch invalidates callbacks from a previous mount.

**Reconnect.** Back off from 500 ms doubling to 5 s, minting a fresh ticket each attempt. After a
reconnect the page resets terminal modes (`ESC[?1000l ?1002l ?1003l ?1006l ?2004l`) so a catch-up
replay cannot leak mouse or bracketed-paste modes into visible text. Keystrokes typed before the
first socket opens are queued (up to 64) and sent on open; a reconnect gap still drops them.

**Parity features** (the "local terminal feel", measured against native and web terminals):

- **Bell:** a dot on the tab by default; badge-plus-sound (an 880 Hz WebAudio beep) or off are
  per-browser preferences.
- **Find:** Ctrl+Shift+F opens a bar (regex, case, whole word; Enter / Shift+Enter; the count comes
  from `onDidChangeResults`, asynchronous and already debounced). Esc closes, clears the
  decorations and refocuses the terminal. The page-level shortcut listener yields the whole
  `.term-holder` subtree, or the bar opens and shuts in one keystroke.
- **Scroll pin:** a user scrolled away stays put while output lands, and a "N new ↓" chip counts
  the lines below; clicking it returns to the bottom. xterm's `onScroll` does not fire for wheel or
  keys, so the pin is judged by a capture-phase wheel listener plus `viewportY >= baseY - 1`, and
  captured before each write. Typing re-pins. Leaving the alternate screen returns to the bottom.
  Never patch xterm internals (`core.scrollToBottom`).
- **Copy on select**, on by default, trims trailing whitespace, and is guarded so search
  navigation (which selects each match) never overwrites the clipboard.
- **Overlay pill:** `cols×rows` on resize, ✂ on copy, the reconnect state.
- **Guidance:** a one-shot orientation hint after the first attach (once per browser) and a `?`
  sheet listing every key and gesture.
- Preferences live in `localStorage` (`swiss.terminal.fontSize`, `.bell`, `.copyOnSelect`,
  `.hint`), wrapped in try/catch.
- The terminal palette reads the panel's `--term-*` tokens and follows the theme switch.

**The Local shell sheet.** A gear beside the target picker opens it; with the local shell off, the
empty state line "Local shell is off — turn it on" opens it too. It has the Enabled switch (with
the one-line reason from §terminal.config) and a Shell field with the probed candidates as
suggestions (any path may be typed). Save reads the current config and revision, changes **only**
`local`, and PUTs the rest back untouched (§host.config's revision lock). With live sessions it
confirms first: "Saving restarts the terminal plugin and closes N sessions." — the honest cost of
`restart_on_config_change`. After the save the targets reload and the local row is selected,
labelled `local · <candidate label>` (basename when no candidate matches).

**Repaint memo.** The tab bar skips identical repaints; any code path that changes the DOM outside
the memo (Escape out of a rename, a remount, a settings save) must reset it, or the bar strands a
rename input or paints empty.

## §terminal.verify — Acceptance and live verification

Acceptance (the numbering is stable; code cites it as "item N"):

1. The gates pass, and `cargo tree -d` shows only `tokio-tungstenite`/`tungstenite` as new.
2. The panel tree check passes (§panel).
3. In a browser, a session on a configured tunnel host logs in, runs `vim` and `htop`, renders CJK
   and emoji without misalignment, and `stty size` follows a window resize.
4. Disabling the tunnels plugin empties the remote list with text naming the tunnels plugin; local
   sessions are unaffected.
5. Disabling the terminal plugin turns every `/api/terminal/*` into a structured 503, and the
   navigation shows it off.
6. A socket dropped for less than the grace window reconnects to the same session with the missing
   output replayed; past the window the session is closed and gone from the list.
7. `yes` for ten seconds, then Ctrl-C: RSS does not climb (backpressure) and the display is intact
   (no lost bytes); the Ctrl-C gets through.
8. Killing `swiss` leaves no shell child behind (ADR-008).
9. The stored recording exports byte-faithfully: `curl -o s.cast` the export route of a
   finished session and `asciinema play s.cast` replays — header, timing and end marker intact.
10. Every §terminal.budget row has a measured number within budget.

**Live verification** runs on a test instance, never on production (§testing.live). Specific to
this page:

- Rebuild with the test target directory and kill the old instance **by its port's owning PID**
  first — a running instance locks the binary and cargo fails late with `os error 5`. After every
  restart read the build hash from `/health`: a restart that silently failed leaves the old binary
  serving.
- **Wait for the prompt before typing.** A cold pwsh start is slow, and the pre-open queue covers
  the socket gap, not a slow child process.
- The matrix: OSC 0 title sync; rename by right-click and by double-click; a manual name beats a
  later OSC title; the bell dot; the find bar opened with focus on the page **and** on the terminal
  (it must stay open); the live match count; find navigation never copies; Esc closes the bar; the
  pin and chip; a multi-line paste confirm; Alt+1/Alt+2 between two tabs; the tab bar after Esc
  from a rename and after navigating away and back; a clean console at the end.
- **The pin test needs choreography:** typing re-pins, so typed output can never show the chip.
  Print ~300 filler lines, run `Start-Sleep 4; <print 25 lines>`, wheel up during the sleep; the
  view must hold, the chip must read `25 new ↓`, and clicking it must return to the bottom.

Automation traps, each of which once produced a wrong verdict:

- A synthetic `dblclick` proves nothing about double-click (it bypasses click sequencing); verify
  that the tab node survives one real click instead.
- Wheel events must target `.xterm-viewport`, not the holder.
- Select the visible terminal (`.term-holder:not([hidden])`); a page with adopted sessions has
  hidden holders of zero size, and assertions on them are vacuous.
- A native `confirm()` parks the renderer; answer it with a raw key press.
- A shortcut is tested twice — focus in the terminal and focus on the page.
- Check that there is scrollback at all (`scrollHeight > clientHeight`) before blaming the pin.
- A test home's recordings are real session output: never commit them.

## §terminal.budget — Budget

| Item | Budget | Measured |
|---|---|---|
| Binary size (the terminal's share, vendored xterm included) | +≤ 800 KB | within budget |
| RSS, plugin loaded with zero sessions | +≤ 1.0 MB | ≈ 0 (in the noise) |
| Each idle remote session | +≤ 256 KB | 250.7 KB |
| Each idle local session | +≤ 1.0 MB | 108 KB plus the read thread's stack (the child process is its own) |
| Catch-up buffer | 64 KB × sessions, a hard cap | a constant, not config |

Measure per §testing.memory. Over budget: first check whether the WebGL addon is the weight, then
whether a buffer lost its bound. The last resorts — the whole plugin behind a cargo feature, or
`portable-pty` for local sessions — each need a §decisions entry, never a quiet swap.

## §terminal.roadmap — Not built yet

The parity work above is P0 and part of P1 of a longer list. What remains, in order of value per
cost:

- **P1:** OSC 52 remote clipboard (addon-clipboard 0.2.0, write-only — refuse the `?` read-back
  query, a clipboard-theft vector — with a 128 KB raw / 75 KB decoded cap, sharing the paste code
  path); OSC 9;4 progress on the tab (addon-progress 0.2.0); a settings sheet for palette, cursor
  and scrollback (1k–50k, default 5000) applied live through `term.options`; a per-tab shell picker
  (the API already takes `shell`).
- **P2, recordings browser:** a panel surface over the terminal history (session list with
  sizes and caps, in-browser replay streaming the stored event lines) — the store (ADR-035)
  makes it a pure read surface; until then the export route is the whole UI.
- **P2, shell integration:** parse OSC 633/133/7 and 9;9 (Windows needs OSC markers — ConPTY
  swallows DCS). It unlocks an exit-code badge (130 = Ctrl-C and 141 = SIGPIPE are not failures),
  the cwd in the tab, Ctrl+↑/↓ prompt navigation, completion notices for long commands, and a
  JSONL command history (2k entries, drop-oldest). Injected scripts fail open to plain passthrough
  for unknown shells, and marker bytes are filtered out of recordings.
- **P3, recordings:** list, download and delete routes; a small in-panel cast player (a JSONL
  scheduler, not the upstream wasm player); `idle_time_limit: 2` in the header; transcript
  download; serialize-snapshot restore on adopt; secret redaction in the recording stream.
- **P4, protocol:** a resumable chunk ring replacing the one-shot catch-up (within the 256 KB
  per-session line); an RTT badge; typeahead for SSH tabs.

Not planned: sixel and images, ligatures and web fonts, zmodem, serial ports, split panes (a large
item needing its own evaluation), collaboration or end-to-end encryption, AI features, desktop
chrome (tray, quake mode, updater).
