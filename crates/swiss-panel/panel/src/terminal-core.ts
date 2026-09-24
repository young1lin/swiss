/*
 * Copyright 2026 young1lin
 * 
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * 
 *     https://www.apache.org/licenses/LICENSE-2.0
 * 
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/* The terminal view's pure half (docs/14 §8): URL building, reconnect pacing, the
   close-frame stories, geometry clamping, the target-picker rows, the Windows-Terminal
   key/mouse actions (docs/15 §1), the font-size zoom steps, the tab-label precedence
   (rename > shell title > target), scroll pinning, paste risk and the bell/copy prefs.
   Plain data in,
   plain data out — the contract with the gateway's Rust side is pinned by the tests in
   test/admin-terminal.test.ts, not by clicking. The DOM/xterm/WS wiring stays in
   views/terminal.js; everything a test needs to trust lives here. */

import type { ApiTerminalSessionRow, ApiTerminalTargets } from "./types/api.js";
import type { TerminalPluginConfig } from "./types/terminal-view.js";
const PTY_MIN = 1;
const PTY_MAX = 1000;   // the bounds swiss-host enforces on both axes — a 0-column PTY is
                      // undefined behaviour on the far side, so the panel clamps BEFORE
                      // the gateway has to refuse (docs/14 §8)

export function targetsUrl(): string { return "/api/terminal/targets"; }
export function sessionsUrl(): string { return "/api/terminal/sessions"; }
export function sessionUrl(id: string): string { return sessionsUrl() + "/" + encodeURIComponent(id); }
export function ticketUrl(id: string): string { return sessionUrl(id) + "/ticket"; }
export function resizeUrl(id: string): string { return sessionUrl(id) + "/resize"; }
export function streamUrl(id: string, ticket: string): string {
  /* Absolute, always: the WebSocket constructor rejects a relative URL with a SyntaxError
     before any connection is attempted. The page origin decides ws/wss; outside a browser
     (node tests) there is no location and the bare path comes back, which keeps this pure. */
  let base = "";
  if (typeof location !== "undefined" && location && location.host) {
    base = (location.protocol === "https:" ? "wss://" : "ws://") + location.host;
  }
  return base + sessionUrl(id) + "/stream?ticket=" + encodeURIComponent(ticket);
}

/** Reconnect pacing after a socket drops. Quick first retry (a lid-close round trip is
 *  short), then doubling to a cap — inside the server's 60 s grace the socket keeps
 *  trying without hammering the mint route. attempt is 0-based. */
export function nextReconnectDelay(attempt: number): number {
  if (!(attempt >= 0)) attempt = 0;
  return Math.min(500 * Math.pow(2, attempt), 5000);
}

/** What the status line says for a server text frame. null = not a control frame we
 *  narrate (binary is the terminal's own bytes; they need no caption). */
export function frameStatus(frame: { t?: string; message?: unknown; code?: number | null } | null | undefined): string | null {
  if (!frame || typeof frame !== "object") return null;
  if (frame.t === "stalled") return "stalled — the client is not accepting output";
  if (frame.t === "error") return frame.message ? String(frame.message) : "closed";
  if (frame.t === "exit") {
    return frame.code == null ? "the shell exited" : "the shell exited with code " + frame.code;
  }
  return null;
}

/** True while the session is still in the gateway's listing — the listing is the truth:
 *  a session that left it is past its grace window, timed out, or was deleted, and no
 *  ticket will ever bring it back (docs/14 §6.7). */
export function sessionAlive(listing: unknown, id: string): boolean {
  return Array.isArray(listing) && listing.some((s) => { return s && s.id === id; });
}

/** Clamp the fit addon's proposal to the PTY bounds before it is SENT, so an honest
 *  resize never becomes a 400. Unknown/absent values fall back to the classics. */
export function clampGeometry(cols: unknown, rows: unknown): { cols: number; rows: number } {
  function axis(v: unknown, fallback: number): number {
    let n = Math.round(Number(v));
    if (!isFinite(n)) n = fallback;
    return Math.min(Math.max(n, PTY_MIN), PTY_MAX);
  }
  return { cols: axis(cols, 80), rows: axis(rows, 24) };
}

/** The resize control frame as the wire wants it: one small JSON object, the only text
 *  the client is allowed to mean anything with (docs/14 §8). */
export function resizeFrame(cols: unknown, rows: unknown): string {
  const g = clampGeometry(cols, rows);
  return JSON.stringify({ t: "resize", cols: g.cols, rows: g.rows });
}

/** Rows for the target picker, in order: local (if the config says so), then every
 *  remote target the shell seat reports. A remote side that is absent explains ITSELF
 *  with the reason the gateway sent — "the tunnels plugin is disabled", never a bare
 *  empty list (docs/14 §4). Returns { rows, note } — note is the one-line story under
 *  the picker when there is nothing remote to pick. */
export function targetRows(reply: ApiTerminalTargets | null | undefined): { rows: { id: string; label: string; state?: string }[]; note: string; reason: string; localOff: boolean } {
  const rows: { id: string; label: string; state?: string }[] = [];
  let note = "";
  const r = reply || {} as ApiTerminalTargets;
  if (r.local && r.local.enabled) {
    rows.push({ id: "local", label: "local · " + localShellLabel(r.local) });
  }
  const remote = r.remote || {} as ApiTerminalTargets["remote"];
  const targets = Array.isArray(remote.targets) ? remote.targets : [];
  targets.forEach((t) => {
    if (!t || !t.id) return;
    const where = t.host ? t.host + (t.port && t.port !== 22 ? ":" + t.port : "") : t.id;
    const who = t.username ? t.username + "@" : "";
    rows.push({
      id: String(t.id),
      label: (t.label || t.id) + " · " + who + where,
      state: t.state || "",
    });
  });
  const reason = remote.presence === "absent" && remote.reason ? String(remote.reason) : "";
  if (!rows.length) {
    note = reason
      || "no terminal targets — connect a tunnel first, or enable the local shell in the plugin config";
  }
  /* localOff turns the empty bar's line into the clickable "turn it on" that opens the
     settings sheet (docs/15 §2.1); reason rides along separately so the tunnels story
     stays visible next to it instead of being buried in the note. */
  const localOff = !!(r.local && r.local.enabled === false);
  return { rows: rows, note: note, reason: reason, localOff: localOff };
}

/** The local picker row's name: the candidates list names the configured shell when it
 *  knows it ("PowerShell 7"); anything else falls back to the program's file name — a
 *  full path in a dropdown is noise, not a name. Windows paths compare
 *  case-insensitively and either slash counts, because a config value may carry both. */
export function localShellLabel(local: { shell?: unknown; shells?: unknown } | null | undefined): string {
  const l = local || {} as { shell?: unknown; shells?: unknown };
  const program = String(l.shell || "");
  const shells = Array.isArray(l.shells) ? l.shells : [];
  for (let i = 0; i < shells.length; i++) {
    const c = shells[i];
    if (c && typeof c.program === "string" && sameProgram(c.program, program)) {
      return String(c.label || baseName(program)) || "shell";
    }
  }
  return baseName(program) || "shell";
}

function sameProgram(a: string, b: string): boolean {
  if (!b) return false;
  return a.split(/[\\/]/).join("/").toLowerCase() === b.split(/[\\/]/).join("/").toLowerCase();
}

function baseName(p: unknown): string {
  const parts = String(p || "").split(/[\\/]/);
  return parts[parts.length - 1] || "";
}

/** The Local shell sheet's config: the CURRENT plugin config with only `local` replaced,
 *  so a save cannot silently drop a limit someone else set (docs/15 §2.1). An empty
 *  shell string is omitted rather than sent as "" — that is how "the platform default"
 *  stays expressible. */
export function withLocalConfig(config: TerminalPluginConfig | null | undefined, enabled: unknown, shell: unknown): TerminalPluginConfig {
  const out: TerminalPluginConfig = Object.assign({}, config || {});
  const local: { enabled: boolean; shell?: string } = { enabled: !!enabled };
  const s = String(shell == null ? "" : shell).trim();
  if (s) local.shell = s;
  out.local = local;
  return out;
}

/** The PUT body for /api/plugins/terminal/config: the config above plus the revision the
 *  GET carried, so a save that raced another panel loses loudly (409) instead of
 *  overwriting it. */
export function configPutBody(config: TerminalPluginConfig | null | undefined, revision: unknown, enabled: unknown, shell: unknown): { config: TerminalPluginConfig; revision: unknown } {
  return { config: withLocalConfig(config, enabled, shell), revision: revision };
}

/** The picker's label for a session tab: short, monospace-friendly, stable. The listing
 *  rows carry the connection's label ("jdoe-demo"); without it a remote tab would show
 *  the raw connection UUID, which is noise, not a name. */
/* The session param is the two fields the label reads: both the wire's listing rows and
   the view's own TermModel carry label/target, and either shape may sit behind a tab. */
export function sessionLabel(session: Pick<ApiTerminalSessionRow, "label" | "target"> | null | undefined): string {
  if (!session) return "?";
  if (session.label) return String(session.label);
  const t = session.target || "?";
  return t === "local" ? "local" : t;
}

/** What one keyboard event means inside the terminal, Windows Terminal style (docs/15
 *  §1): "paste" | "copy" | "sigint" | null, where null means "not ours — xterm keeps the
 *  event". Takes a duck-typed { key, code, ctrlKey, shiftKey, altKey, metaKey, type }
 *  rather than a KeyboardEvent so a test can press any combination without a DOM, and
 *  judges keydown ONLY: xterm hands keypress and keyup to the custom handler too, and
 *  deciding on those would fire every action twice. Ctrl+C is the load-bearing
 *  asymmetry — with a selection it copies (and must NOT reach the shell as ^C), without
 *  one it stays the interrupt a flooding program is counting on. */
export function keyAction(ev: { type?: string; key?: string; ctrlKey?: boolean; shiftKey?: boolean; altKey?: boolean; metaKey?: boolean } | null | undefined, hasSelection: boolean): string | null {
  if (!ev || ev.type !== "keydown") return null;
  const key = String(ev.key || "").toLowerCase();
  /* Alt-combos the terminal page owns while a terminal holds focus (Windows Terminal's
     tab jumps). Judged BEFORE the blanket Alt pass-through below so Alt+V and friends
     stay untouched: only digits, the horizontal arrows and Alt+W are ours. The
     browser's own Ctrl+Shift+W / Ctrl+Tab / Ctrl+Shift+T cannot be intercepted from a
     page, so close/cycle/new ride Alt instead — the web-panel reality desktop
     terminals do not face. */
  if (ev.altKey && !ev.metaKey && !ev.ctrlKey) {
    if (/^[1-9]$/.test(key)) return "tab-" + key;
    if (key === "arrowleft") return "tab-prev";
    if (key === "arrowright") return "tab-next";
    if (key === "w") return "tab-close";
    return null;
  }
  if (ev.metaKey || ev.altKey) return null;   // mac paste and the browser's own menu combos
  if (ev.ctrlKey) {
    if (key === "f" && ev.shiftKey) return "search";                // Ctrl+Shift+F, the web-terminal classic
    if (key === "v") return "paste";                                // Ctrl+V, Ctrl+Shift+V
    if (key === "c") {
      if (ev.shiftKey) return hasSelection ? "copy" : null;         // Ctrl+Shift+C
      return hasSelection ? "copy" : "sigint";                      // Ctrl+C
    }
    if (key === "insert") return hasSelection ? "copy" : null;      // Ctrl+Insert
    /* Font zoom, Windows Terminal's keys: Ctrl+= / Ctrl++ (the shifted = and the numpad
       +), Ctrl+- / Ctrl+_ , Ctrl+0 back to the default. Claimed here so the terminal
       grows instead of the whole page — a browser-zoomed panel is the complaint this
       answers, not the feature. */
    if (key === "=" || key === "+") return "zoom-in";
    if (key === "-" || key === "_") return "zoom-out";
    if (key === "0") return "zoom-reset";
    return null;
  }
  if (ev.shiftKey && key === "insert") return "paste";              // Shift+Insert
  return null;
}

/** Font sizes the zoom steps through: the default, the floor below which cells stop
 *  being glyphs, and a ceiling that still fits a prompt on a laptop. */
export const FONT_DEFAULT = 13;
export const FONT_MIN = 8;
export const FONT_MAX = 32;

/** The next font size for one zoom action, clamped: "zoom-in" | "zoom-out" | "zoom-reset";
 *  anything else — or a size that is not a number — comes back as the default, so a
 *  corrupt stored value can never wedge the terminal at 0px. */
export function nextFontSize(current: unknown, action: string): number {
  const size = readFontSize(current);
  if (action === "zoom-in") return Math.min(FONT_MAX, size + 1);
  if (action === "zoom-out") return Math.max(FONT_MIN, size - 1);
  return FONT_DEFAULT;
}

/** A stored font size back into a usable one: an integer inside the bounds, or the
 *  default. localStorage hands back strings, and older panels stored nothing. */
export function readFontSize(raw: unknown): number {
  const n = Math.round(Number(raw));
  if (!isFinite(n) || n < FONT_MIN || n > FONT_MAX) return FONT_DEFAULT;
  return n;
}

/** What a Ctrl+wheel over the terminal means: "zoom-in" scrolling up, "zoom-out"
 *  scrolling down, null for a plain scroll (which stays xterm's scrollback). */
export function wheelAction(ev: { ctrlKey?: boolean; deltaY?: number } | null | undefined): string | null {
  if (!ev || !ev.ctrlKey || !ev.deltaY) return null;
  return ev.deltaY < 0 ? "zoom-in" : "zoom-out";
}

/** What a right-click on the terminal surface means: paste with no selection, copy one
 *  away, and Shift+right-click keeps the browser's context menu as the escape hatch. Only
 *  button === 2 is judged — the wiring subscribes to contextmenu, whose button is the one
 *  that opened it. */
export function mouseAction(ev: { button?: number; shiftKey?: boolean } | null | undefined, hasSelection: boolean): string | null {
  if (!ev || ev.button !== 2) return null;
  if (ev.shiftKey) return "menu";
  return hasSelection ? "copy" : "paste";
}

/** The label one tab shows, in the precedence every native terminal uses: a manual
 *  rename wins, then the shell's own OSC 0/2 title, then the target the session was
 *  opened on. An EMPTY shell title is the shell resetting its title, not a label —
 *  fall through. Seven of nine explored reference terminals converged on exactly
 *  this order (docs/22 consensus 1). */
export function tabLabel(session: Pick<ApiTerminalSessionRow, "label" | "target"> | null | undefined, shellTitle: unknown, customTitle: unknown): string {
  const custom = customTitle == null ? "" : String(customTitle).trim();
  if (custom) return custom;
  const shell = shellTitle == null ? "" : String(shellTitle).trim();
  if (shell) return shell;
  return sessionLabel(session);
}

/** Pinned-to-bottom judgment while output streams in (Tabby's rule, docs/22 §2.10):
 *  within one line of the base means the user is riding the bottom. Unknown values
 *  count as pinned — a wrongly-pinned terminal merely scrolls; a wrongly-unpinned
 *  one yanks the user's scrollback, which is the failure this exists to prevent. */
export function isPinned(viewportY: unknown, baseY: unknown): boolean {
  const v = Number(viewportY), b = Number(baseY);
  if (!isFinite(v) || !isFinite(b)) return true;
  return v >= b - 1;
}

/** How many newlines a paste would type BEYOND the one trailing Enter that ends a
 *  normal paste. CRLF and lone CR both count as one. One trailing newline is stripped
 *  first: pasting "ls\n" is how every paste ends and is not a second command. The
 *  caller gates on the alternate screen (vim) where multiline is the norm. */
export function embeddedNewlines(text: unknown): number {
  const s = String(text == null ? "" : text).replace(/\r\n?/g, "\n").replace(/\n$/, "");
  let n = 0;
  for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) === 10) n++;
  return n;
}

/** A stored bell mode back into a usable one: "badge" (default — a bell must be
 *  VISIBLE by default, the repo's defaults-on rule), "badge-sound", or "off". */
export const BELL_BADGE = "badge", BELL_BADGE_SOUND = "badge-sound", BELL_OFF = "off";
export function readBellMode(raw: unknown): string {
  return raw === BELL_BADGE_SOUND || raw === BELL_OFF ? raw : BELL_BADGE;
}

/** Copy-on-select preference. Defaults ON (iTerm2's stance; the ✂ overlay makes the
 *  copy visible so it is not a surprise) — "off" is the escape hatch. */
export function readCopyOnSelect(raw: unknown): boolean {
  return raw === "off" ? false : true;
}

/** Trailing whitespace trimmed from a selection before it hits the clipboard: spaces
 *  and tabs at line ends, plus the final newline a rectangular drag usually grabs. */
export function trimSelection(text: unknown): string {
  return String(text == null ? "" : text).replace(/[ \t]+(?=\n)/g, "").replace(/\s+$/, "");
}

/* --- the xterm theme's chrome (docs/46 P8-2, U12) ----------------------------------------------
 * The theme splits in two: the CHROME (background, cursor, selection) reads the panel's
 * --term-* tokens (base.css) through getComputedStyle, so the surface follows a theme
 * switch like every other surface in the panel; the 16 ANSI colours are CONTENT - what a
 * program printed is not chrome to re-skin - and stay ttyd's literal set, tuned as a whole
 * with the foreground (borrowing half of a tuned set is how you get a terminal that looks
 * almost right). The reader is injected so the split, not the DOM, is what the tests pin. */
export type TermTokenRead = (name: string) => string;

/** The browser's reader: --term-* off the document element, where base.css defines them
 *  (light) and :root[data-theme=dark] overrides two of them. A missing or empty token
 *  falls back to the light block's own value - a blank canvas is never the answer. */
export function readTermTokens(name: string): string {
  const cs = getComputedStyle(document.documentElement);
  const v = cs.getPropertyValue(name);
  return v == null ? "" : v.trim();
}

export function termTheme(read: TermTokenRead = readTermTokens) {
  const token = (name: string, fallback: string): string => {
    const v = read(name);
    return v || fallback;
  };
  return {
    foreground: "#d2d2d2", background: token("--term-bg", "#17181b"),
    cursor: token("--term-cursor", "#e4e4e7"),
    selectionBackground: token("--term-sel", "rgba(59, 130, 246, 0.35)"),
    black: "#000000", red: "#d81e00", green: "#5ea702", yellow: "#cfae00",
    blue: "#427ab3", magenta: "#89658e", cyan: "#00a7aa", white: "#dbded8",
    brightBlack: "#686a66", brightRed: "#f54235", brightGreen: "#99e343",
    brightYellow: "#fdeb61", brightBlue: "#84b0d8", brightMagenta: "#bc94b7",
    brightCyan: "#37e6e8", brightWhite: "#f1f1f0",
  };
}

/** The theme-switch hook's body: re-read the tokens and hand every OPEN terminal the new
 *  chrome. Passed the live models' term objects (anything with an options.theme), so the
 *  observer in views/terminal.ts stays one line of DOM and this stays testable. */
export function applyTermTheme(terms: Array<{ options: { theme?: unknown } }>, read: TermTokenRead = readTermTokens): void {
  const theme = termTheme(read);
  for (const t of terms) t.options.theme = theme;
}
