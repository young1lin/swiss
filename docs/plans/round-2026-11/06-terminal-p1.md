# 06 — Terminal: a shell picker per tab, appearance preferences, OSC 52 copy

Implementation brief, not SPEC. The SPEC amendments in §6 land in the commit that makes each
paragraph true. Shared rules: [00-handoff.md](00-handoff.md). Panel-only (no Rust change is
expected; if one turns out to be needed, stop and report). Needs 01 for its strings.

## 1. Facts (verified 2026-11-12)

- The server already takes a per-session shell: `POST /api/terminal/sessions` accepts
  `{target, cols, rows, shell?}` (`crates/swiss-plugins/src/terminal_api.rs:233-283`;
  SPEC §terminal.api: "`shell` is an optional one-session override of the local program").
  `GET /api/terminal/targets` answers `local {enabled, shell, shells: [{program, label}]}` — the
  candidates probed at plugin start, and `shell`, the resolved default.
- The panel never sends `shell`: `views/terminal.ts:998` POSTs `{target, cols, rows}`.
- The bar (`views/terminal.ts:1060-1085`) has the target `<select id="term-target">`, the gear
  `iconBtn("gear", …, {id: "term-set"})` which opens `openLocalSheet()` (the Local shell
  **server** config sheet, `views/terminal-settings.ts`), and `btn("Open session", {id: "term-new"})`.
- xterm options at `views/terminal.ts:543-548`: `fontSize`, `scrollback: 5000` (fixed),
  `fontFamily`, `theme: termTheme()` (`terminal-core.ts:348`, built from CSS tokens; a theme
  change re-applies to every open terminal at `terminal-core.ts:369`).
- Font size is already a per-browser preference: `swiss.terminal.fontSize` in localStorage, read
  through try/catch (`views/terminal.ts:77-89, 207-218`), changed by Ctrl+wheel/keys, applied
  live to every open terminal via `m.term.options.fontSize`.
- Vendored: xterm 5.5 + fit, search, unicode11, web-links, webgl. **No clipboard addon** —
  OSC 52 goes through `term.parser.registerOscHandler(52, handler)`, which is public API.
- `toast(msg, isErr?)` (`util.ts:121`) is text-only. `copyText()` (`ui/copy.ts:30`) tries the
  async clipboard, then `execCommand("copy")`, and returns false when both refuse. Browsers may
  refuse a clipboard write that has no user gesture behind it.
- `views/terminal.ts` is 1275 lines. New code goes into new files (§2.2, §2.3), not into it.

## 2. Work items

### T1 — choose the shell when opening a local session

- When the selected target is local and `targets.local.shells.length > 1`, a small chevron
  button sits beside Open session (a split button: the main part opens the default as today,
  the chevron opens a `popupMenu` listing each shell's `label`, the default marked).
  Choosing one opens a session with that `program` as `shell`.
- The last explicit choice is remembered per browser (`swiss.terminal.lastShell`, try/catch),
  and only if it is still in `shells` on the next load; the main button keeps opening the
  configured default — the memory only pre-marks the menu. (Changing the default stays the
  server sheet's job.)
- The tab's title shows the chosen shell's label when it differs from the default.
- Remote targets: no chevron, and `shell` is never sent.
- Keyboard: the chevron is a real button with an accessible name ("Choose a shell"), the menu
  follows the existing `popupMenu` keyboard behaviour.

### T2 — appearance preferences

New module `terminal-prefs.ts`: one typed `TerminalPrefs` object in localStorage key
`swiss.terminal.prefs` (JSON; parse errors and missing fields fall back per field; every read and
write in try/catch; the page works with storage throwing).

| Pref | Values | Default | Applies via |
| --- | --- | --- | --- |
| `scrollback` | integer, clamped 1000–50000 | 5000 | `term.options.scrollback` |
| `cursorStyle` | `block` / `bar` / `underline` | `block` | `term.options.cursorStyle` |
| `cursorBlink` | bool | `false` (today the option is unset, which is xterm's `false`) | `term.options.cursorBlink` |
| `palette` | `auto` / `dark` / `light` / `contrast` | `auto` | `term.options.theme` |
| `fontSize` | the existing key and clamp | unchanged | unchanged |
| `osc52` | bool | `true` | T3 |

- `fontSize` keeps its existing key `swiss.terminal.fontSize`, so nobody's zoom resets. The
  appearance sheet just shows and edits it through the existing `setFontSize`.
- `palette: auto` is exactly today's `termTheme()` (tokens of the panel theme). `dark`, `light`,
  `contrast` are fixed palettes defined in `terminal-core.ts` beside `termTheme`; `contrast` meets
  WCAG AA for foreground/background and for each ANSI colour on the background (assert it in a test
  with a small contrast helper).
- Changes apply **live** to every open terminal, and to other open panel tabs through the
  `storage` event (re-read and re-apply; ignore events for other keys).
- Not relayed through any gateway state in v1 — a preference about this screen.
- The gear becomes a menu (`popupMenu`) with two items: "Local shell…" (the existing
  `openLocalSheet`) and "Appearance…" (a new sheet built with the house field sheet / form
  helpers). When local is off and the bar shows `term-off`, the appearance item is still
  reachable from the gear if the gear is rendered; if it is not rendered in that state, leave it —
  do not add a second entry point.

### T3 — OSC 52: a program may put text on the clipboard

- Register `term.parser.registerOscHandler(52, handler)` on every terminal the page creates
  (local and remote), in a new `terminal-osc52.ts` exporting a pure `parseOsc52(data)` and the
  handler factory.
- `data` is `<selection>;<payload>`. Accept any selection letters (`c`, `p`, `s`, `0`–`7`, empty)
  and treat them all as the clipboard. `payload === "?"` is a **read request: never answered**,
  return `true` (handled, nothing sent back). The clipboard is never read, ever.
- Limits: raw `data` over 128 KiB → ignored. Decode base64 (standard alphabet, padding optional,
  whitespace stripped); invalid → ignored. Decoded bytes over 75 KiB → ignored. Decode as UTF-8
  with `TextDecoder("utf-8", {fatal: false})`. Empty → ignored.
- When `osc52` pref is off → ignored (still return `true`, so xterm does not print it).
- Otherwise `copyText(text)`. On success, `toast(tr("terminal.osc52Copied", {n, from}))` where
  `n` is the character count and `from` the tab's label — every write is visible, because a
  remote host can now write the clipboard. On failure (no gesture, permission), open a small
  sheet: "<from> asked to copy N characters", a mono preview of the first 200 characters, a
  Copy button (the click is the gesture) and Dismiss. At most one such sheet; a newer request
  replaces its text.
- Ignored requests are silent (no toast), but count them in a debug-only console line at most.

## 3. Out of scope

OSC 9;4 progress, shell integration (OSC 133/633), paste-side features, per-target palettes,
syncing prefs across browsers.

## 4. Tests (vitest, `--maxWorkers=2`, touched files; `npm run check` once at the end)

- T1: the chevron appears only for local with >1 shells; choosing a shell POSTs
  `{target, cols, rows, shell: <program>}`; the main button POSTs no `shell`; remote never sends
  it; a remembered shell missing from `shells` is dropped; storage throwing does not break the bar.
- T2: defaults; clamping (`scrollback: 10` → 1000, `999999` → 50000, junk → 5000); a partial
  stored object keeps the good fields; applying a pref sets `options` on every open terminal (fake
  terminals); a `storage` event re-applies; the contrast palette passes AA; the gear menu opens
  both sheets.
- T3: `parseOsc52` — valid, `?`, invalid base64, over each limit, UTF-8 multibyte, selection
  letters; the handler with the pref off copies nothing; on copy failure the sheet opens and its
  button calls the copy again; **no code path reads the clipboard** (assert
  `navigator.clipboard.readText` is never touched — make it a spy that throws).

## 5. Live proof (19998)

Open a local PowerShell session and a second one with another shell from the chevron (title
shows it). In the Appearance sheet set scrollback 1000, cursor bar, palette light — the open
session changes at once; open a second browser tab, change the palette, the first follows.
For OSC 52, in the local session run a one-liner that prints `ESC ] 52 ; c ; <base64 of "hello
swiss"> BEL` (PowerShell: ``[Console]::Write("`e]52;c;aGVsbG8gc3dpc3M=`a")``), see the toast,
paste into the panel's search box to prove it. Then `ESC ] 52 ; c ; ? BEL` — nothing happens and
nothing is written back to the shell. Screenshots of the chevron menu, the Appearance sheet, and
the toast.

## 6. SPEC and docs in the same commit

- §terminal.api: the panel now sends `shell` for local sessions chosen from the menu (the route is
  unchanged).
- §panel.pages (Terminal): the split button, the gear menu, the Appearance sheet and its prefs
  (per browser, localStorage, applied live), and the OSC 52 rule: write-only, limits, every write
  toasts, a pref turns it off, `?` is never answered.
- §security (chapter 0015), the terminal paragraph: a program in a session — including on a remote
  host — can write the clipboard through OSC 52; it cannot read it; the toast and the pref.
- CHANGELOG `[Unreleased]`: Added ×3.

## 7. Delivery

Three commits, one per item, each with its tests, strings, SPEC lines and emit:
`feat(panel): choose the shell when opening a local terminal`,
`feat(panel): terminal appearance preferences`,
`feat(panel): OSC 52 lets a terminal program copy to the clipboard`.
