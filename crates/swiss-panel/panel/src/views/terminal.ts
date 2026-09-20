/*
 * Copyright 2026 The swiss authors
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

/* ================================================================================================
   Terminal - the panel half of the terminal plugin (docs/14 §2, §8).

   One real terminal per session: xterm.js (vendored, native ES modules via shims) over
   the gateway's WebSocket. The wire contract is exactly docs/14 §8: binary frames are
   raw PTY bytes both ways, the only text is a small control object, and a socket that
   drops does NOT end the session - the gateway holds it for the grace window and the
   catch-up bytes arrive on reconnect. The reconnect loop mints a fresh one-time ticket
   each round (the one from open lives 10 s; the grace lives 60 s - that asymmetry is
   the whole reason POST .../ticket exists).

   The pure half of this page (URLs, pacing, close stories, geometry clamping, picker
   rows) lives in ../terminal-core.js and is pinned by test/admin-terminal.test.ts;
   the Local shell settings sheet lives in ./terminal-settings.js.
   ================================================================================================ */
import type { ApiTerminalOpened, ApiTerminalSessionRow, ApiTerminalTargets } from "../types/api.js";
import type { TermModel, TerminalPackages } from "../types/terminal-view.js";
import { $, api, apiJson, errText, iconNode, targetEl, toast } from "../util.js";
import { fill, frag, h } from "../h.js";
import type { HChild } from "../h.js";
import { loadXterm } from "../vendor/xterm/xterm-5.5.0/index.js";
import { loadFitAddon } from "../vendor/xterm/addon-fit-0.10.0/index.js";
import { loadUnicode11Addon } from "../vendor/xterm/addon-unicode11-0.8.0/index.js";
import { loadWebLinksAddon } from "../vendor/xterm/addon-web-links-0.11.0/index.js";
import { loadWebglAddon } from "../vendor/xterm/addon-webgl-0.18.0/index.js";
import {
  FONT_DEFAULT, clampGeometry, embeddedNewlines, frameStatus, isPinned, keyAction, mouseAction,
  nextFontSize, nextReconnectDelay, readBellMode, readCopyOnSelect, readFontSize, resizeFrame,
  resizeUrl, sessionAlive, sessionsUrl, streamUrl, tabLabel, targetRows,
  targetsUrl, ticketUrl, trimSelection, wheelAction,
} from "../terminal-core.js";
import { createOverlay } from "../term-overlay.js";
import { loadSearchAddon } from "../vendor/xterm/addon-search-0.16.0/index.js";
import { openLocalSheet } from "./terminal-settings.js";
import { closeSheet } from "../add-sheet.js";
import { tr } from "../i18n.js";

/* docs/14 §2: the system monospace stack - no Nerd Font, no web font. The resource
   pipeline is text-only; a font file cannot enter the tree, by design. */
const FONT = 'ui-monospace, SFMono-Regular, Consolas, "Cascadia Mono", monospace';

let packages: Promise<TerminalPackages> | null = null;             // the vendored constructors, loaded once per module lifetime
export let targets: ApiTerminalTargets | null = null;       // the last /api/terminal/targets reply (a live binding: terminal-settings.js reads it)
export let sessions: ApiTerminalSessionRow[] = [];        // the last listing (rows without local state; exported as a live binding too)
let models: TermModel[] = [];       // sessions this mount has wired a terminal for
let active: string | null = null;     // the session id whose terminal is on stage
let epoch = 0;         // mount generation: loops and sockets from an older mount stop
let seq = 0;           // temp ids for sessions opened but not yet answered
let fitTimer: ReturnType<typeof setTimeout> | null = null;
/* The terminal's own font size, zoomed with Ctrl+=/-/0 or Ctrl+wheel and remembered
   per browser — a preference about this screen, not gateway state, so localStorage
   like the theme. Read once at module load; a blocked store just means 13px. */
const FONT_SIZE_KEY = "swiss.terminal.fontSize";
let fontSize = readFontSize(readStoredFontSize());
/* Sessions the user dismissed. The gateway keeps a closed row in the listing until its
   grace window lapses; without this set, every repaint would resurrect a tab the user
   has already closed, and dismissing would look decorative. Cleared of ids the listing
   no longer carries so it cannot grow without bound. */
let dismissed = new Set<string>();
/* P0 preferences (docs/22 §2.3/§2.4): a bell must be VISIBLE by default and a
   selection must reach the clipboard by default — both remember their mode per
   browser, like the font size, because they are preferences about this screen. */
const BELL_KEY = "swiss.terminal.bell";
const COPYSEL_KEY = "swiss.terminal.copyOnSelect";
function storedPref(key: string) { try { return localStorage.getItem(key); } catch (e) { return null; } }

/* One-shot guidance (docs/22 P0 guidance layer): the first attach is the one moment a
   newcomer is guaranteed to be looking at the terminal, so it carries one sentence of
   orientation and never appears again. */
const HINT_KEY = "swiss.terminal.hint";
const bellMode = readBellMode(storedPref(BELL_KEY));
const copyOnSelect = readCopyOnSelect(storedPref(COPYSEL_KEY));
let bellAudio: AudioContext | null = null;   // the AudioContext, created by the first audible bell

function load() {
  if (!packages) {
    packages = Promise.all([
      loadXterm(), loadFitAddon(), loadUnicode11Addon(), loadWebLinksAddon(), loadWebglAddon(),
    ]).then((got) => {
      /* Key names are the CONSTRUCTORS wireTerminal news up (got.Terminal, got.FitAddon,
         got.Unicode11Addon, got.WebLinksAddon, got.WebglAddon) - a mismatch here is
         "X is not a constructor" at first Open, which is exactly how it once shipped. */
      return { Terminal: got[0], FitAddon: got[1], Unicode11Addon: got[2], WebLinksAddon: got[3], WebglAddon: got[4] };
    });
  }
  return packages;
}

function sleep(ms: number) { return new Promise((r) => { setTimeout(r, ms); }); }

function model(id: string | null) { return models.find((m) => { return m.id === id; }) || null; }

/* The status foot under the surface: a state dot that answers before the words do,
   then one sentence for whichever session is on stage. The tone classes come from the
   status strings this file and the gateway's control frames produce. */
function statusTone(text: string) {
  if (!text) return "";
  if (/attached/.test(text)) return "ok";
  if (/connecting|opening|reconnecting/.test(text)) return "warn";
  if (/closed|exited|gone|stalled|not running|failed|could not/.test(text)) return "bad";
  return "";
}

function paintStatus() {
  const line = $("term-status");
  if (!line) return;
  const m = model(active);
  const text = m ? m.status : "";
  line.hidden = false;   // the foot always caps the card; an empty strip is still its shape
  line.textContent = "";
  if (text) {
    const dot = document.createElement("span");
    dot.className = "term-dot " + statusTone(text);
    line.appendChild(dot);
    line.appendChild(document.createTextNode(text));
  }
  /* A zoomed terminal says so, and how to get back - the size is remembered across
     reloads, so without this line a 20px terminal next week would look like a bug. */
  if (fontSize !== FONT_DEFAULT) {
    const zoom = document.createElement("span");
    zoom.className = "term-zoom";
    zoom.textContent = fontSize + "px \u00b7 Ctrl+0 resets";
    line.appendChild(zoom);
  }
}

/* The session tabs. A tab exists for every wired model plus every live listing row the
   user has not opened yet; a closed session keeps its tab (the scrollback is readable)
   until dismissed. */
/* paintTabs' last-markup memo, module-scoped (docs/37 M3): it once rode on the function
   object itself (paintTabs.last), typed by a Function augmentation. Null means "the bar's
   DOM was replaced under us - repaint even if the markup is textually equal". */
let paintTabsLast: string | null = null;

function paintTabs() {
  const bar = $("term-tabs");
  if (!bar) return;
  const all = tabList();
  const any = all.length > 0;
  bar.hidden = !any;
  /* Rebuilding identical markup rips the nodes out mid-double-click (see select) and
     would destroy an open rename input. The memo is the built tree's own outerHTML
     (docs/37 R5): same tree in, same markup out - skip. */
  const tabs = all.map((m) => {
    const label = tabLabel(m, m.shellTitle, m.customTitle) + (m.gone ? tr(" · closed") : "");
    return h("button", { role: "tab", data: { act: "select", id: m.id },
        aria: { selected: String(m.id === active) }, title: label },
      h("span", { class: "term-tab-label" }, label),
      m.bell && !m.gone ? h("span", { class: "term-tab-bell", aria: { label: tr("bell") } }, "\u25cf") : null,
      " ",
      h("span", { class: "term-tab-x", data: { act: "close", id: m.id }, title: tr("Close session"), role: "button" }, "\u00d7"));
  });
  const html = tabs.map((n) => { return n.outerHTML; }).join("");
  if (html === paintTabsLast) return;
  paintTabsLast = html;
  fill(bar, tabs);
}

function paintStage() {
  const stage = $("term-stage");
  if (!stage) return;
  const empty = $("term-empty");
  if (!model(active)) {
    if (empty) empty.hidden = false;
  } else if (empty) {
    empty.hidden = true;
  }
  Array.prototype.forEach.call(stage.querySelectorAll("[data-term]"), (holder) => {
    holder.hidden = holder.getAttribute("data-term") !== active;
  });
}

function readStoredFontSize() {
  try { return localStorage.getItem(FONT_SIZE_KEY); } catch (e) { return null; }
}

/* Apply one font size to every wired terminal and refit the one on stage - the others
   are refitted when they are selected, exactly as after a window resize. */
function setFontSize(size: number) {
  if (size === fontSize) return;
  fontSize = size;
  try { localStorage.setItem(FONT_SIZE_KEY, String(size)); } catch (e) { /* per-tab only */ }
  models.forEach((m) => {
    if (!m.term) return;
    m.term.options.fontSize = size;
    /* A zoom across odd sizes can leave the WebGL glyph atlas half-rasterized; one
       clear rebuilds it on demand (the addon's own advice, docs/22 §4). */
    if (m.gl && m.gl.clearTextureAtlas) m.gl.clearTextureAtlas();
  });
  scheduleFit();
  paintStatus();
}

/* Copy the selection and clear it — Windows Terminal's semantics: the next Ctrl+C must
   be the interrupt again. writeText works on http://127.0.0.1 (a potentially-trustworthy
   origin); the rare refusal gets a sentence rather than silence. */
function copySelection(term: XtermTerminal) {
  const text = term.getSelection();
  navigator.clipboard.writeText(text).catch(() => {
    toast(tr("could not write the selection to the clipboard"), true);
  });
  term.clearSelection();
}

/* Create the terminal for one session and append its holder. The holder stays hidden
   until the session is selected - xterm keeps its buffer offscreen, fit measures only
   what is visible. */
/* --- P0 native-feel machinery (docs/22) ------------------------------------------------ */

/* A short generated beep: no asset file can enter the panel (docs/14 §2), and xterm
   5.5 ships no sound of its own (verified, docs/22 §3). The context is created on the
   first audible bell and left alone afterwards — an idle one costs nothing. */
function beep() {
  try {
    /* The legacy vendor spelling, cast locally: no lib declares webkitAudioContext and the
       global Window augmentation is retired (docs/37 M3). The || keeps the runtime honest -
       a browser with neither name throws here exactly as it always did. */
    const audioCtor = window.AudioContext ||
      (window as Window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext as typeof AudioContext | undefined;
    bellAudio = bellAudio || new audioCtor();
    const osc = bellAudio.createOscillator();
    const gain = bellAudio.createGain();
    osc.frequency.value = 880;
    gain.gain.value = 0.05;
    osc.connect(gain);
    gain.connect(bellAudio.destination);
    osc.start();
    gain.gain.exponentialRampToValueAtTime(0.0001, bellAudio.currentTime + 0.18);
    osc.stop(bellAudio.currentTime + 0.2);
  } catch (e) { /* autoplay policy: the badge already spoke */ }
}

/* Every byte that reaches a screen goes through here so pin state survives it: the
   viewport is captured BEFORE the write and restored after it — the bottom when
   pinned, the saved line when not. Capture-before-write matters because an rAF can
   flip the pin mid-write (Tabby's rule, docs/22 §2.10). */
function writeTerm(m: TermModel, bytes: Uint8Array) {
  const term = m.term as XtermTerminal;
  if (!term) return;
  const b = term.buffer.active;
  const was = m.pinned;
  const y = b.viewportY;
  term.write(bytes, () => {
    if (!m.term) return;
    if (was && m.toBottom) m.toBottom();
    else if (!was) term.scrollToLine(Math.min(y, term.buffer.active.baseY));
  });
}

/* The "N new \u2193" chip over a terminal the user has scrolled away from (docs/22
   §2.10): one button, absolutely positioned, honest about how far behind they are. */
function paintJump(m: TermModel) {
  if (!m.holder) return;
  if (!m.jump) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "term-jump";
    chip.addEventListener("click", () => {
      m.unseen = 0;
      if (m.toBottom) m.toBottom();
      paintJump(m);
    });
    m.holder.appendChild(chip);
    m.jump = chip;
  }
  m.jump.hidden = m.pinned || !m.unseen;
  m.jump.textContent = m.unseen + " new \u2193";
}

/* The tab inventory paintTabs and the Alt-shortcuts agree on: wired models first
   (their order IS the tab order), then live listing rows not opened yet. */
function tabList() {
  const known = models.map((m) => { return m.id; });
  const extra = sessions.filter((s) => {
    return s && s.id && !known.includes(s.id) && !dismissed.has(s.id);
  });
  return models.concat(extra.map((s) => {
    return { id: s.id, target: s.target, label: s.label, status: "", gone: false } as TermModel;
  }));
}

/* Alt+1..9 / Alt+arrows out of a focused terminal (docs/22 §2.5). The browser keeps
   Ctrl+Tab and Ctrl+Shift+W for itself — no page can have them — so Alt carries the
   set; the web-panel reality desktop terminals do not face. */
function jumpTab(at: number) {
  const all = tabList();
  if (at < 0 || at >= all.length) return;
  select(all[at].id);
}

function cycleTab(dir: number) {
  const all = tabList();
  if (all.length < 2) return;
  let at = -1;
  for (let i = 0; i < all.length; i++) if (all[i].id === active) { at = i; break; }
  if (at < 0) return;
  select(all[(at + dir + all.length) % all.length].id);
}

/* Double-click renames a tab: one inline input replaces the label; Enter commits,
   Escape cancels, blur commits (docs/22 consensus 1 + §2.5). A custom title outranks
   the shell's OSC title until it is emptied. */
function startRename(id: string) {
  const m = model(id) as TermModel;
  const bar = $("term-tabs");
  if (!m || !bar) return;
  const btn = Array.prototype.find.call(bar.children, (c) => {
    return c.getAttribute("data-id") === id;
  });
  const labelEl = btn && btn.querySelector(".term-tab-label");
  if (!labelEl || btn.querySelector(".term-rename")) return;
  const input = document.createElement("input");
  input.className = "term-rename";
  input.id = "term-rename";   // the a11y auditor wants a name on every form field
  input.maxLength = 40;
  input.value = m.customTitle || m.shellTitle || "";
  input.setAttribute("aria-label", "Rename tab");
  labelEl.parentNode.replaceChild(input, labelEl);
  input.focus();
  input.select();
  let settled = false;
  const done = (commit: boolean) => {
    if (settled) return;
    settled = true;
    if (commit) m.customTitle = input.value.trim() || null;
    /* The bar was mutated outside paintTabs (replaceChild above), so an unchanged
       model - Escape, or a commit equal to the current label - would recompute the
       identical string and the skip would strand this input inside the tab forever
       (fresh-eyes audit B1b). Reset the memo; the repaint then always runs. */
    paintTabsLast = null;
    paintTabs();
    if (m.term) m.term.focus();   // the repaint ate the input that held the focus
  };
  input.addEventListener("keydown", (ev) => {
    ev.stopPropagation();   // the terminal's key gate must not see rename typing
    if (ev.key === "Enter") done(true);
    else if (ev.key === "Escape") done(false);
  });
  input.addEventListener("blur", () => { done(true); });
}

/* --- Ctrl+Shift+F find bar (docs/22 P1) ------------------------------------------------ */
/* The addon CLASS loads on first ask — an idle terminal never pays the ~78 KB — and one
   SearchAddon instance attaches per terminal, because a match set belongs to a buffer.
   The addon re-searches internally as the buffer changes under it and reports counts
   through onDidChangeResults, async and ~200 ms debounced INSIDE it; adding another
   debounce on top is the classic integrator mistake — don't. Plain substring search
   only (regex off): a pasted "[" stays a "[" and never throws. */
const FIND_DECOR = {
  matchOverviewRuler: "#427ab3",
  matchBackground: "rgba(66,122,179,0.35)",
  activeMatchBackground: "#cfae00",
  activeMatchOverviewRuler: "#cfae00",
};

function findBar() { return $("term-find"); }
function findInput() { return $<HTMLInputElement>("term-find-q"); }

function paintFindCount(res: { resultIndex: number; resultCount: number } | null | undefined) {
  const el = $("term-find-count");
  if (!el) return;
  el.textContent = !res ? "" : res.resultCount ? (res.resultIndex + 1) + "/" + res.resultCount : "no results";
}

function runFind(back: boolean) {
  const m = model(active);
  if (!m || !m.search) return;
  const input = findInput();
  const q = input ? input.value : "";
  if (!q) { m.search.clearDecorations(); paintFindCount(null); return; }
  try {
    /* findNext/findPrevious SELECT the match they move to, which fires
       onSelectionChange - without this flag, copy-on-select would overwrite the
       clipboard with the matched substring on every keystroke in the find input
       (fresh-eyes audit B3). The selection change fires synchronously inside the
       call, so a try/finally bracket is exactly the right width. */
    m.suppressSelect = true;
    const moved = back ? m.search.findPrevious(q, { decorations: FIND_DECOR })
                     : m.search.findNext(q, { decorations: FIND_DECOR });
    if (!moved) paintFindCount(null);
  } catch (e) { /* only reachable with regex on; the guard stays because search must never kill the page */ }
  finally { m.suppressSelect = false; }
}

function closeFind() {
  const bar = findBar();
  if (bar) bar.hidden = true;
  const m = model(active);
  if (m && m.search) m.search.clearDecorations();
  paintFindCount(null);
  if (m && m.term) m.term.focus();
}

/* The ? reference sheet: the guidance layer's third tier (docs/22 P0). Keys and mouse
   gestures in the panel's own sheet component - read-only, Esc or backdrop closes, no
   new surfaces invented. kbd is monospace because a key is a value you would copy
   (design rule 1). */
function openHelpSheet() {
  /* Rows take NODES now (docs/37 R5): a key combo is an element pair, not a string that
     happens to hold markup. */
  const row = (keys: HChild, what: string) => {
    return h("div", { class: "term-key-row" }, h("span", { class: "term-key-k" }, keys), h("span", null, what));
  };
  const cap = (title: string) => { return h("div", { class: "term-key-cap" }, title); };
  const k = (t: string) => { return h("kbd", null, t); };
  // Visible before the paint (panel-proof-of-life rule 1).
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label: tr("Terminal shortcuts") } },
      h("div", { class: "sheet-head" }, h("h2", null, tr("Terminal shortcuts"))),
      h("div", { class: "sheet-body term-key-body" },
        cap(tr("Keys")),
        row([k("Ctrl+Shift+F")], tr("find in this session’s buffer")),
        row([k("Enter"), k("Shift+Enter")], tr("next / previous match")),
        row([k("Esc")], tr("close the find bar")),
        row([k("Alt+1..9")], tr("switch to tab 1..9")),
        row([k("Alt+\u2190"), k("Alt+\u2192")], tr("previous / next tab")),
        row([k("Alt+W")], tr("close the tab")),
        row([k("Ctrl+0")], tr("reset the font size (Ctrl+wheel zooms)")),
        row([k("?")], tr("this sheet, anywhere on the page")),
        cap(tr("Mouse")),
        row(tr("double-click / right-click a tab"), tr("rename it (the name outranks the shell\u2019s title)")),
        row(tr("middle-click a tab"), tr("close it")),
        row(tr("drag a selection"), tr("copied on release; Ctrl+C stays the interrupt")),
        row(tr("scroll up"), tr("stop following output \u2014 a chip counts what you missed")),
        cap(tr("On by default")),
        row(tr("bell"), tr("a dot on the tab until you read it")),
        row(tr("copy-on-select"), tr("a scissors pill confirms each copy")),
        row(tr("multiline paste"), tr("asks first \u2014 paste whole or not at all"))),
      h("div", { class: "sheet-foot" }, h("span", { class: "grow" }), h("button", { class: "btn", id: "th-close" }, tr("Close")))));
  const onKey = (ev: KeyboardEvent) => { if (ev.key === "Escape") close(); };
  const close = () => {
    document.removeEventListener("keydown", onKey, true);
    closeSheet();
  };
  document.addEventListener("keydown", onKey, true);
  $("th-close").onclick = close;
  $("sheet").onclick = (ev) => { if (ev.target === $("sheet")) close(); };
}

/* Ctrl+Shift+F while the terminal page is staged, even when the terminal itself does
   not hold focus - xterm's own handler only sees keys that reach its textarea, and a
   user who just clicked the tab bar or the page is left without a shortcut (VS Code
   binds find at the view level for exactly this reason). Capture phase; real fields
   keep their keys. The terminal is deliberately EXCLUDED: xterm's custom key handler
   binds the same chord, and xterm's _keyDown consults that handler without ever
   checking defaultPrevented - a document-capture call on top would double-fire
   openFind, whose toggle then closes the bar again (fresh-eyes audit B2). */
function pageFindShortcut(ev: KeyboardEvent) {
  const el = targetEl(ev);
  const tag = el && el.tagName;
  if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
  if (el && el.closest(".term-holder")) return;   // xterm owns keys in here
  if (!$("term-find")) return;   // some other page is staged
  if (ev.key === "?") {          // the reference sheet - "?" asks a question
    ev.preventDefault();
    openHelpSheet();
    return;
  }
  if (!ev.ctrlKey || !ev.shiftKey || ev.altKey || ev.metaKey) return;
  if (String(ev.key || "").toLowerCase() !== "f") return;
  ev.preventDefault();
  void openFind();
}

function wireFindBar() {
  const bar = findBar();
  const input = findInput();
  if (!bar || !input) return;
  input.addEventListener("keydown", (ev) => {
    ev.stopPropagation();   // typing a query is not terminal input
    if (ev.key === "Enter") { ev.preventDefault(); runFind(ev.shiftKey); }
    else if (ev.key === "Escape") { ev.preventDefault(); closeFind(); }
  });
  /* Typing searches immediately (VS Code's behavior) — Enter then WALKS the matches. */
  input.addEventListener("input", () => { runFind(false); });
  $("term-find-prev").addEventListener("click", () => { runFind(true); });
  $("term-find-next").addEventListener("click", () => { runFind(false); });
  $("term-find-x").addEventListener("click", closeFind);
}

async function openFind() {
  const m = model(active);
  if (!m || !m.term) return;
  const bar = findBar();
  if (!bar) return;
  if (!bar.hidden && document.activeElement === findInput()) { closeFind(); return; }   // toggle
  bar.hidden = false;
  if (!m.search) {
    try {
      const cls = await loadSearchAddon();
      if (!m.term || m.gone) { closeFind(); return; }   // closed while the class loaded
      m.search = new cls();
      m.search.onDidChangeResults((res) => { paintFindCount(res); });
      m.term.loadAddon(m.search);
    } catch (e) {
      toast("could not load the search addon: " + errText(e), true);
      closeFind();
      return;
    }
  }
  const input = findInput();
  input.focus();
  input.select();
  runFind(false);   // searching on open makes "open, type" work with zero extra keys
}

function wireTerminal(m: TermModel) {
  if (m.term) return Promise.resolve();
  // The promise is module-cached, so a rejected load retries naturally on the next wire.
  return load().then((got) => {
    const term = new got.Terminal({
      fontFamily: FONT,
      fontSize: fontSize,
      scrollback: 5000,
      allowProposedApi: true,   // terminal.unicode (the Unicode11 table) is a proposed API
      theme: termTheme(),
    });
    const holder = document.createElement("div");
    holder.className = "term-holder";
    holder.setAttribute("data-term", m.id);
    $("term-stage").appendChild(holder);
    term.open(holder);
    const fit = new got.FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new got.Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.loadAddon(new got.WebLinksAddon());
    // WebGL is a pure-JS addon (verified at vendoring time: no wasm anywhere in it), but
    // a machine without WebGL must still get a terminal - the DOM renderer is the
    // fallback, and a lost GL context falls back to it mid-session the same way.
    try {
      const gl = new got.WebglAddon();
      gl.onContextLoss(() => { gl.dispose!(); });
      term.loadAddon(gl);
      m.gl = gl;   // kept for clearTextureAtlas() after a font zoom
    } catch (e) { /* the DOM renderer stays */ }
    term.onData((text) => { sendInput(m, text); });
    /* OSC 0/2 from the shell (vim, ssh, pwsh prompts) drives this tab's label unless the
       user renamed it (docs/22 consensus 1). ConPTY forwards the sequence; a shell that
       never emits one simply keeps its session label. */
    term.onTitleChange((title) => {
      m.shellTitle = title == null ? null : String(title);
      paintTabs();
    });
    /* BEL: a badge on the tab, cleared by selecting it; an optional oscillator beep —
       xterm 5.5 has no bell sound of its own (verified, docs/22 §3). */
    term.onBell(() => {
      if (m.gone) return;
      m.bell = true;
      paintTabs();
      if (bellMode === "badge-sound") beep();
    });
    /* Copy-on-select rides the same clipboard path as Ctrl+C; the \u2702 overlay is the
       feedback that keeps the copy from being a surprise (ttyd's lesson). suppressSelect
       is set while search navigation moves the selection programmatically. */
    term.onSelectionChange(() => {
      if (!copyOnSelect || m.suppressSelect) return;
      const text = term.getSelection();
      if (!text) return;
      navigator.clipboard.writeText(trimSelection(text)).catch(() => { /* gesture context missing: keep silent */ });
      if (m.overlay) m.overlay.show("\u2702", 500);
    });
    /* Windows Terminal's key story, not xterm's Linux default (docs/15 §1): without this
       handler xterm turns Ctrl+V into the ^V control byte and the shell sees nothing
       pasted. Returning false skips xterm's own handling — and nothing else: the browser
       then delivers its native paste event to the focused .xterm-helper-textarea, whose
       listener xterm already installs, so the text rides onData to the shell exactly as
       a Ctrl+Shift+V always did (verified on 19998 before this was written). Deliberately
       no navigator.clipboard.readText() here — the native event needs no permission prompt. */
    term.attachCustomKeyEventHandler((ev) => {
      const action = keyAction(ev, term.hasSelection());
      if (action === "paste") return false;   // no preventDefault: the browser paste IS the payload
      if (action === "copy") { copySelection(term); return false; }
      if (action === "zoom-in" || action === "zoom-out" || action === "zoom-reset") {
        ev.preventDefault();   // or the browser zooms the whole page on the same keys
        setFontSize(nextFontSize(fontSize, action));
        return false;
      }
      if (action === "search") { ev.preventDefault(); void openFind(); return false; }
      if (action === "tab-close") {
        ev.preventDefault();
        void closeSession(active!);
        return false;
      }
      if (action === "tab-prev" || action === "tab-next") {
        ev.preventDefault();
        cycleTab(action === "tab-next" ? 1 : -1);
        return false;
      }
      if (action && action.indexOf("tab-") === 0) {
        ev.preventDefault();
        jumpTab(parseInt(action.slice(4), 10) - 1);
        return false;
      }
      return true;   // "sigint" and everything else stay xterm's business
    });
    /* Ctrl+wheel zooms the terminal, not the page; a plain wheel stays scrollback.
       Not passive: preventDefault is the whole point when Ctrl is down. */
    holder.addEventListener("wheel", (ev) => {
      const action = wheelAction(ev);
      if (!action) return;
      ev.preventDefault();
      setFontSize(nextFontSize(fontSize, action));
    }, { passive: false });
    /* Right-click pastes, or copies a selection away; Shift+right-click keeps the
       browser's menu as the escape hatch (docs/15 §1). */
    holder.addEventListener("contextmenu", (ev) => {
      const action = mouseAction(ev, term.hasSelection());
      if (action === "menu") return;
      ev.preventDefault();
      if (action === "copy") { copySelection(term); term.focus(); return; }
      /* term.paste() is xterm's public API and rides the same bracketed-paste path as
         the keyboard. readText may prompt for clipboard access once; a refusal must not
         strand the click silently. */
      navigator.clipboard.readText().then((text) => {
        term.paste(text);
        term.focus();   // the click landed on the surface, not the keyboard focus
      }).catch(() => {
        toast(tr("the browser refused to read the clipboard — use Ctrl+V"), true);
        term.focus();
      });
    });
    /* Pin-to-bottom (docs/22 §2.10). Deliberately NO patch of xterm internals: live
       verification on the 19996 instance proved xterm 5.5's output auto-follow is the
       buffer natively tracking the bottom while ydisp rides it — scrollToBottom is not
       the path, so patching it disables nothing. Worse, the write path CALLS it
       mid-frame, and a patch that flips m.pinned there poisons the very next write's
       captured state (view yanked, chip never shown). The whole mechanism therefore
       rides the PUBLIC API: writeTerm captures the viewport before each write and
       restores it after (bottom when pinned, the saved line when not), and pin state
       comes only from the wheel — capture phase, decided immediately, re-read in rAF. */
    m.toBottom = () => {
      m.pinned = true;
      if (m.term) m.term.scrollToBottom();
    };
    holder.addEventListener("wheel", (ev) => {
      if (ev.deltaY < 0) m.pinned = false;   // leaving the bottom is a decision, made now
      requestAnimationFrame(() => {
        if (!m.term) return;
        const b = m.term.buffer.active;
        m.pinned = isPinned(b.viewportY, b.baseY);
      });
    }, { capture: true, passive: true });
    term.onLineFeed(() => {
      if (m.pinned) return;
      m.unseen += 1;   // the chip is for the reader who scrolled away, not the rider
      paintJump(m);
    });
    /* CSI 2026 (synchronized output) h...l paired with CSI 3 J is a full-screen repaint
       — the moment right after quitting vim where the reader must land on the bottom
       again (Wave's trick, docs/22 §2.10). Both handlers only observe; false lets xterm
       keep processing. */
    term.parser.registerCsiHandler({ prefix: "?", params: [2026], final: "h" }, () => {
      m.sync2026 = Date.now();
      return false;
    });
    term.parser.registerCsiHandler({ prefix: "?", params: [2026], final: "l" }, () => {
      if (Date.now() - m.sync2026 < 2000) m.repaint2026 = true;
      return false;
    });
    term.parser.registerCsiHandler({ params: [3], final: "J" }, () => {
      if (m.repaint2026) {
        m.repaint2026 = false;
        if (m.toBottom) m.toBottom();
      }
      return false;
    });
    /* Multiline paste confirmation (docs/22 consensus 8): a capture listener on the
       holder sees the paste BEFORE xterm's textarea listener, and stopPropagation keeps
       xterm out of it entirely. Only a paste that would type Enter mid-text asks; the
       alternate screen (vim) never does — multiline is the norm there. */
    holder.addEventListener("paste", (ev) => {
      const text = ev.clipboardData && ev.clipboardData.getData("text/plain");
      if (typeof text !== "string" || !text) return;
      const lines = embeddedNewlines(text);
      if (!lines) return;
      if (term.buffer.active.type === "alternate") return;
      ev.preventDefault();
      ev.stopPropagation();
      const preview = text.length > 1000 ? text.slice(0, 1000) + "\u2026" : text;
      if (window.confirm("Paste " + (lines + 1) + " lines into the shell?\n\n" + preview)) {
        term.paste(text);
      }
    }, true);
    m.term = term;
    m.fit = fit;
    m.holder = holder;
    m.overlay = createOverlay(holder);   // resize geometry, copy feedback, attach states
  });
}

/* A terminal wears its own palette whatever the panel theme is doing — this is ttyd's
   xterm theme, used verbatim because it is the most-copied xterm.js palette and its
   bg/fg/cursor and 16 ANSI colors are tuned as a set (borrowing half of it is how you
   get a terminal that looks almost right). */
function termTheme() {
  return {
    foreground: "#d2d2d2", background: "#2b2b2b", cursor: "#adadad",
    black: "#000000", red: "#d81e00", green: "#5ea702", yellow: "#cfae00",
    blue: "#427ab3", magenta: "#89658e", cyan: "#00a7aa", white: "#dbded8",
    brightBlack: "#686a66", brightRed: "#f54235", brightGreen: "#99e343",
    brightYellow: "#fdeb61", brightBlue: "#84b0d8", brightMagenta: "#bc94b7",
    brightCyan: "#37e6e8", brightWhite: "#f1f1f0",
  };
}

/* The shared encoder is made on first use; the const capture keeps the narrowed type
   inside the send closures (a let's narrowing does not survive into one). */
let encoder: TextEncoder | null = null;
function sendInput(m: TermModel, text: string) {
  if (!m.ws || m.ws.readyState !== 1) {
    /* Keystrokes typed while the FIRST socket is still coming up ride exactly once
       (VS Code's pre-launch queue, docs/22 §4 P0 item 8); a reconnect gap still drops them —
       replaying keys into a shell that may have moved on is worse than losing them. */
    if (m.buffered && m.buffered.length < 64) m.buffered.push(text);
    return;
  }
  const enc = encoder || (encoder = new TextEncoder());
  m.ws.send(enc.encode(text));
  if (m.toBottom) m.toBottom();   // typing puts the reader back on the bottom
}

/* Connect one session with a ticket. The ticket is spent by the gateway BEFORE the 101,
   so a refused one arrives as onclose with an HTTP story - the reconnect loop then finds
   the session either alive (mint again) or gone (say so). */
function connect(m: TermModel, ticket: string) {
  const ws = new WebSocket(streamUrl(m.id, ticket));
  ws.binaryType = "arraybuffer";   // default would wrap every frame in a Blob
  m.ws = ws;
  m.status = "connecting\u2026";
  paintStatus();
  const my = epoch;
  ws.onopen = () => {
    if (my !== epoch) return;
    m.attempt = 0;
    m.status = "attached";
    paintStatus();
    if (m.buffered && m.buffered.length) {
      const enc = encoder || (encoder = new TextEncoder());
      const queued = m.buffered;
      m.buffered = null;   // the pre-launch queue rides once, then never again
      queued.forEach((t) => { ws.send(enc.encode(t)); });
    }
    if (m.term) {
      /* A re-attach must not leak the previous attach's mouse-tracking or
         bracketed-paste modes into the catch-up replay as visible escape text
         (docs/22 §4 P0 item 8 — Tabby's reconnect reset, adapted to swiss's grace window). */
      m.term.write("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l");
      if (m.overlay) m.overlay.show("attached", 700);
    /* Guidance tier 2: once per browser, three seconds after the first attach, one
       sentence of orientation. The attached pill has cleared by then. */
    if (!storedPref(HINT_KEY)) {
      try { localStorage.setItem(HINT_KEY, "1"); } catch (e) { /* per-tab only */ }
      setTimeout(() => {
        if (m.overlay) m.overlay.show("double-click a tab to rename · ? lists everything", 6000);
      }, 900);
    }
    }
    scheduleFit();
  };
  ws.onmessage = (event) => {
    if (my !== epoch) return;
    if (typeof event.data === "string") {
      let frame = null;
      try { frame = JSON.parse(event.data); } catch (e) { return; }
      const story = frameStatus(frame);
      if (story) { m.status = story; paintStatus(); }
    } else if (m.term) {
      writeTerm(m, new Uint8Array(event.data));
    }
  };
  ws.onclose = () => {
    m.ws = null;
    if (my !== epoch || m.userClosed || m.gone) return;
    void reconnect(m);
  };
}

/* The grace-window half of the page: keep trying while the gateway still lists the
   session, back off between rounds, and stop the moment the story is final. Uses raw
   api() rather than apiJson() because a 503/404 here is an ANSWER, not something to
   toast about on every round. */
async function reconnect(m: TermModel) {
  if (m.userClosed || m.gone) return;
  const my = epoch;
  m.status = "reconnecting\u2026";
  paintStatus();
  for (;;) {
    await sleep(nextReconnectDelay(m.attempt));
    m.attempt += 1;
    if (my !== epoch || m.userClosed || m.gone) return;

    const listed = await api(sessionsUrl());
    if (my !== epoch) return;
    if (listed.status === 503) { return gone(m, "the terminal plugin is not running"); }
    if (!listed.ok) { continue; }
    const listing = await listed.json().catch(() => { return []; });
    if (!sessionAlive(listing, m.id)) {
      return gone(m, "the session closed while the socket was down (grace window, idle or stall timeout)");
    }

    const minted = await api(ticketUrl(m.id), { method: "POST" });
    if (my !== epoch) return;
    if (minted.status === 404) { return gone(m, "the session closed while the socket was down"); }
    if (minted.status === 503) { return gone(m, "the terminal plugin is not running"); }
    if (!minted.ok) { continue; }
    const body = await minted.json().catch(() => { return {}; });
    if (body.ticket) { connect(m, body.ticket); return; }
  }
}

function gone(m: TermModel, story: string) {
  m.gone = true;
  m.status = story;
  paintStatus();
  paintTabs();
}

/* Fit the on-stage terminal and tell the gateway. Two channels on purpose (docs/14
   §8): the resize frame while the socket is up, POST /resize while it is down - the
   panel's window may have changed during exactly that gap. */
function fitNow() {
  fitTimer = null;
  const m = model(active);
  if (!m || !m.fit || !m.term) return;
  let dims = null;
  try { dims = m.fit.proposeDimensions(); } catch (e) { return; }
  if (!dims || !dims.cols || !dims.rows) return;
  const g = clampGeometry(dims.cols, dims.rows);
  m.fit.fit();
  if (m.gone || !m.id || m.id.indexOf("pending-") === 0) return;
  if (g.cols === m.sentCols && g.rows === m.sentRows) return;
  if (m.overlay) m.overlay.show(g.cols + "\u00d7" + g.rows, 700);   // ttyd's resize toast
  m.sentCols = g.cols;
  m.sentRows = g.rows;
  if (m.ws && m.ws.readyState === 1) {
    m.ws.send(resizeFrame(g.cols, g.rows));
  } else {
    void api(resizeUrl(m.id), { method: "POST", body: JSON.stringify({ cols: g.cols, rows: g.rows }) });
  }
}

function scheduleFit() {
  if (fitTimer) return;
  fitTimer = setTimeout(fitNow, 150);
}

/* Selecting a tab: wire the terminal if it is the first look, stage it, and refit -
   the container was hidden or absent when the terminal was created. */
function select(id: string) {
  /* Re-clicking the ACTIVE tab is deliberately a no-op for painting: paintTabs
     rebuilds the bar's innerHTML, and a rebuild between the two clicks of a
     double-click makes the second land on a fresh node - no dblclick ever fires,
     and rename lives on dblclick. Just take the focus back. */
  if (id === active) {
    const cur = model(id);
    /* Answering the bell must not repaint the bar either - same dblclick rule as
       above - so the badge node is dropped surgically instead. */
    if (cur && cur.bell) {
      cur.bell = false;
      const barEl = $("term-tabs");
      const dot = barEl && barEl.querySelector('button[data-id="' + id + '"] .term-tab-bell');
      if (dot) dot.remove();
    }
    if (cur && cur.term) cur.term.focus();
    return;
  }
  active = id;
  const seen = model(id);
  if (seen && seen.bell) seen.bell = false;   // selecting a tab answers its bell
  paintTabs();
  paintStage();
  let m = model(id) as TermModel;
  if (!m) {
    const row = sessions.find((s) => { return s && s.id === id; });
    if (!row) return;
    // label from the listing row: without it the tab falls back to the raw target
    // UUID - exactly what a second page adopting this session used to show.
    m = {
      id: id, target: row.target, label: row.label, status: "connecting\u2026", attempt: 0,
      userClosed: false, gone: false, sentCols: 0, sentRows: 0,
      shellTitle: null, customTitle: null, bell: false, pinned: true, unseen: 0,
      buffered: [], sync2026: 0, repaint2026: false, suppressSelect: false, search: null,
    };
    models.push(m);
    paintTabs();
  }
  void wireTerminal(m).then(() => {
    if (active !== m.id) return;
    paintStage();
    if (!m.ws && !m.gone && !m.userClosed) void resume(m);
    scheduleFit();
    if (m.term) m.term.focus(); // switching tabs types into the one you switched to
    const fb = findBar();   // an open find bar follows the tab: its matches belong to a buffer
    if (fb && !fb.hidden) { if (m.search) runFind(false); else paintFindCount(null); }
  }).catch((error) => {
    toast(String(error && error.message || error), true);
  });
}

/* Attach to a session that already exists (returning to the page, or a session opened
   in another browser tab): mint a ticket and connect. */
async function resume(m: TermModel) {
  const my = epoch;
  const minted = await api(ticketUrl(m.id), { method: "POST" });
  if (my !== epoch || m.ws || m.gone || m.userClosed) return;
  if (minted.status === 404 || minted.status === 503) { return gone(m, minted.status === 503 ? "the terminal plugin is not running" : "the session is gone"); }
  if (!minted.ok) { m.status = "could not reach the gateway"; paintStatus(); return; }
  const body = await minted.json().catch(() => { return {}; });
  if (body.ticket) connect(m, body.ticket);
}

/* Open a fresh session: create the terminal first so fit can measure a real container,
   then open with the measured geometry. */
async function openSession() {
  const pick = $<HTMLSelectElement>("term-target");
  if (!pick || !pick.value) return;
  const my = epoch;
  // The short tab label comes from the picker row ("jdoe-demo"), not the target id -
  // a remote session without this showed its raw connection UUID on the tab.
  const row = targetRows(targets).rows.find((r) => { return r.id === pick.value; });
  const m: TermModel = {
    id: "pending-" + (++seq), target: pick.value, label: row ? String(row.label).split(" \u00b7 ")[0] : "",
    status: "opening\u2026",
    attempt: 0, userClosed: false, gone: false, sentCols: 0, sentRows: 0,
    shellTitle: null, customTitle: null, bell: false, pinned: true, unseen: 0,
    buffered: [], sync2026: 0, repaint2026: false, suppressSelect: false, search: null,
  };
  models.push(m);
  active = m.id;
  paintTabs();
  paintStage();
  try {
    await wireTerminal(m);
  } catch (e) {
    /* A vendored package that failed to load must say so - a silently vanishing tab teaches
       the user that Open is decorative. */
    toast("could not load the terminal packages: " + errText(e), true);
    models.splice(models.indexOf(m), 1);
    active = null;
    paintTabs();
    paintStage();
    return;
  }
  if (my !== epoch) return;
  paintStage();
  let g = { cols: 80, rows: 24 };
  try {
    const dims = m.fit?.proposeDimensions();
    if (dims && dims.cols && dims.rows) g = clampGeometry(dims.cols, dims.rows);
    m.fit?.fit();
  } catch (e) { /* the defaults already stand */ }
  m.status = "opening\u2026";
  paintStatus();
  const reply = await apiJson<ApiTerminalOpened>(sessionsUrl(), {
    method: "POST",
    body: JSON.stringify({ target: m.target, cols: g.cols, rows: g.rows }),
  });
  if (my !== epoch) return;
  if (!reply || !reply.id) {   // the toast said why; drop the shell we staged
    if (m.term) { m.term.dispose(); m.term = null; }   // in-flight write callbacks land on the guard, not a disposed terminal
    if (m.holder) m.holder.remove();
    models.splice(models.indexOf(m), 1);
    active = models.length ? models[models.length - 1].id : null;
    paintTabs();
    paintStage();
    paintStatus();
    return;
  }
  /* The model's id changes here (pending-N -> the gateway's id); everything keyed on
     the id must follow in the same breath. active is the one that is easy to forget,
     and forgetting it kills the status line AND hides the holder for every fresh
     open: model(active) stops resolving, paintStatus paints "", and paintStage's
     data-term match hides all holders. Found by the browser gate's status trail. */
  const wasActive = active === m.id;
  m.id = String(reply.id);
  if (wasActive) active = m.id;
  m.holder?.setAttribute("data-term", m.id);
  paintTabs();
  paintStage();
  if (reply.ticket) connect(m, reply.ticket);
  /* Keyboard focus moves INTO the new terminal. It stayed on the Open button, so the Enter
     a user reaches for to get a prompt opened a second session on the same host instead -
     and a third pressed the per-target cap. Every real terminal focuses the shell it opens. */
  if (m.term && active === m.id) m.term.focus();
}

/* Closing a tab: a live session is DELETEd (the gateway writes the visible reason into
   the stream before it closes); a closed one is only removed locally - the session is
   already gone, a DELETE would just be a 404. */
async function closeSession(id: string) {
  const m = model(id);
  dismissed.add(id);   // stays dismissed until the listing itself drops the row
  if (m && m.term) { m.term.dispose(); m.term = null; }   // same guard as the open-failure path
  if (m && m.holder) { m.holder.remove(); }
  models = models.filter((x) => { return x.id !== id; });
  if (active === id) active = models.length ? models[models.length - 1].id : null;
  paintTabs();
  paintStage();
  paintStatus();
  if (!m || m.gone) return;
  m.userClosed = true;
  if (m.ws) { try { m.ws.close(); } catch (e) { /* already gone */ } }
  await apiJson(sessionsUrl() + "/" + encodeURIComponent(id), { method: "DELETE" });
}

/* The page: one bar (tabs left, picker + Open right), the dark surface, the status
   foot. No pane-head, no description paragraph — the nav tab already says Terminal,
   and every real web terminal spends its top row on tabs, not on prose. */
function render() {
  const pane = $("pane");
  if (!pane) return;
  // term-host turns the pane into the definite-height flex column the page fills exactly
  // (views.css); unmount() takes it back off so no other page inherits the layout.
  pane.classList.add("term-host");
  const pick = targetRows(targets);
  fill(pane,
    h("div", { class: "term-page" },
      h("div", { class: "term-bar" },
        h("div", { class: "term-tabs", id: "term-tabs", role: "tablist", aria: { label: tr("Sessions") }, hidden: true }),
        h("div", { class: "term-ctl" },
          pick.rows.length
            /* The select closes after its options and the two buttons are its SIBLINGS
               (the master string builder spelled it exactly so): a <button> inside a
               <select> is invalid DOM and the browser drops it - the Open session
               button vanished exactly this way once. */
            ? frag(
                h("select", { id: "term-target", class: "term-pick", aria: { label: tr("Target") } },
                  pick.rows.map((r) => {
                    return h("option", { value: r.id }, r.label + (r.state ? " (" + r.state + ")" : ""));
                  })),
                /* The Local shell settings entry (docs/15 §2.1): a quiet gear beside the
                   picker, not a second loud button — Open session stays the bar's one accent.
                   The gear is the sprite (i-gear), never a Unicode glyph (design rule 9). */
                h("button", { class: "term-gear", id: "term-set", title: tr("Local shell settings"), aria: { label: tr("Local shell settings") } }, iconNode("gear")),
                h("button", { class: "btn term-new", id: "term-new" }, tr("Open session")))
            /* Local off and nothing to pick: the line itself is the way in (docs/15
               §2.1) — a dead-end note that names a setting nobody can reach is how the
               gap this sheet closes came to exist. The tunnels reason, when there is one,
               stays readable beside it. */
            : pick.localOff
              ? frag(
                  h("button", { class: "term-off", id: "term-off" }, tr("Local shell is off — turn it on")),
                  pick.reason ? h("span", { class: "term-none" }, pick.reason) : null)
              : h("span", { class: "term-none" }, pick.note),
          /* The ? reference button (guidance tier 3): same quiet box as the gear - present
             in every branch, including the empty ones, where it matters most. The empty slot
             after it accepts the shell-owned app zone only during Terminal fullscreen; there
             is no duplicate page-owned fullscreen control (immersive.js). */
          h("button", { class: "term-gear", id: "term-help", title: tr("Shortcuts and gestures"), aria: { label: tr("Shortcuts and gestures") } }, iconNode("help"))),
        /* term-bar ENDS here (the second paren below closes it): master's term-page owns
           [bar, find, stage, foot] as siblings - an unclosed bar once swallowed find+stage,
           the stage went 0px tall inside the fixed-height bar, and xterm rendered nothing. */
        h("span", { class: "term-shell-slot", data: { "shell-focus-slot": "" } })),
      h("div", { class: "term-find", id: "term-find", hidden: true },
        h("input", { id: "term-find-q", type: "text", placeholder: tr("Find"), aria: { label: tr("Find in terminal") }, spellcheck: false }),
        h("span", { class: "term-find-count", id: "term-find-count" }),
        h("button", { type: "button", id: "term-find-prev", title: tr("Previous match (Shift+Enter)") }, "\u2191"),
        h("button", { type: "button", id: "term-find-next", title: tr("Next match (Enter)") }, "\u2193"),
        h("button", { type: "button", id: "term-find-x", title: tr("Close (Esc)") }, "\u00d7")),
      h("div", { class: "term-stage", id: "term-stage" },
        h("div", { class: "term-empty", id: "term-empty", hidden: true },
          h("div", { class: "term-ghost", aria: { hidden: "true" } },
            h("span", { class: "term-ghost-dollar" }, "$"),
            h("span", { class: "term-ghost-cursor" })),
          h("h2", null, tr("No session yet")),
          h("p", null, tr("Pick a host in the bar above and open one.")),
          /* The one teaching moment every newcomer sees: three facts, one line, quiet. */
          h("p", { class: "term-keys-hint" },
            h("kbd", null, "Ctrl+Shift+F"), tr(" find · "),
            h("kbd", null, "Alt+1..9"), tr(" switch · "),
            h("kbd", null, "?"), tr(" everything")),
          h("p", null, tr("A dropped socket does not end a session \u2014 it waits out the grace window and catches up.")))),
      h("div", { class: "term-foot", id: "term-status" })));

  wireFindBar();
  document.addEventListener("keydown", pageFindShortcut, true);
  const button = $("term-new");
  if (button) button.onclick = () => { void openSession(); };
  const gear = $("term-set");
  if (gear) gear.onclick = () => { void openLocalSheet(); };
  const help = $("term-help");
  if (help) help.onclick = openHelpSheet;
  const off = $("term-off");
  if (off) off.onclick = () => { void openLocalSheet(); };
  const tabs = $("term-tabs");
  if (tabs) {
    tabs.onclick = (event) => {
      const closer = targetEl(event)?.closest('[data-act="close"]');
      if (closer) { void closeSession(closer.getAttribute("data-id")!); return; }
      const tab = targetEl(event)?.closest('[data-act="select"]');
      if (tab) select(tab.getAttribute("data-id")!);
    };
    tabs.ondblclick = (event) => {
      const tab = targetEl(event)?.closest('[data-act="select"]');
      if (tab) startRename(tab.getAttribute("data-id")!);
    };
    tabs.onauxclick = (event) => {
      if (event.button !== 1) return;   // middle-click closes (Tabby / native terminals)
      const tab = targetEl(event)?.closest('[data-act="select"]');
      if (tab) void closeSession(tab.getAttribute("data-id")!);
    };
    tabs.oncontextmenu = (event) => {
      const tab = targetEl(event)?.closest('[data-act="select"]');
      if (!tab) return;
      event.preventDefault();   // the browser menu has nothing to say about a session tab
      startRename(tab.getAttribute("data-id")!);
    };
  }
  window.addEventListener("resize", scheduleFit);
  /* The fill above replaced the whole pane - a fresh, EMPTY tab bar - so the memo
     from the previous paint is a lie here. Without this reset, a settings save
     (reload -> render) with unchanged models skips the repaint and blanks the bar
     (fresh-eyes audit B1a). */
  paintTabsLast = null;
  paintTabs();
  paintStage();
  if (!active && sessions.length) select(sessions[0].id);
  else if (!active) { const empty = $("term-empty"); if (empty) empty.hidden = false; }
}

export async function reload() {   // exported for terminal-settings.js (a save refreshes the page through it)
  const t = await apiJson<ApiTerminalTargets>(targetsUrl());
  if (t) targets = t;
  const r = await api(sessionsUrl());
  if (r.status === 503) {
    sessions = [];
  } else if (r.ok) {
    const body = await r.json().catch(() => { return []; });
    sessions = Array.isArray(body) ? body : [];
  }
  // A dismissed id the listing no longer carries will never come back; forget it.
  const listed = new Set(sessions.map((s) => { return s && s.id; }));
  for (const d of Array.from(dismissed)) if (!listed.has(d)) dismissed.delete(d);
  render();
}

/* The shell has no unmount hook; mounting again is the only signal that the last
   page's terminals are unreachable. Without this, every visit away and back leaked the
   previous mount whole: the WebSockets stayed open (so the gateway's grace clock never
   started - the session read as attached forever) and each xterm kept its canvases and
   its WebGL context for the life of the tab. The gateway sessions themselves stay -
   re-entry adopts them again; only the client-side resources are released here. */
function releaseModels() {
  models.forEach((m) => {
    if (m.ws) { try { m.ws.close(); } catch (e) { /* already gone */ } }
    if (m.term) { try { m.term.dispose(); } catch (e) { /* already gone */ } }
    if (m.holder) { m.holder.remove(); }
    m.ws = null; m.term = null; m.holder = null;
  });
}

export async function mount() {
  epoch += 1;               // stale callbacks from the last mount die here
  releaseModels();
  models = [];
  active = null;
  dismissed = new Set();   // a fresh mount knows nothing of the last page's tabs
  await reload();
}

export async function refresh() { await reload(); }

/** The shell's slow cadence: republish the listing so tabs pick up bytesOut and the
 *  attached dot, and let a staged-but-unwired listing row stay honest. */
export async function poll() {
  if (!$("term-stage")) return;
  const r = await api(sessionsUrl());
  if (r.status === 503) return;   // the navigation chrome already carries the story
  if (!r.ok) return;
  const body = await r.json().catch(() => { return []; });
  sessions = Array.isArray(body) ? body : [];
  const listed = new Set(sessions.map((s) => { return s && s.id; }));
  for (const d of Array.from(dismissed)) if (!listed.has(d)) dismissed.delete(d);
  paintTabs();
}

export function countText() {
  return sessions.length ? sessions.length + " live" : "";
}

export function unmount() {
  epoch += 1;
  const pane = $("pane");
  if (pane) pane.classList.remove("term-host");
  if (fitTimer) { clearTimeout(fitTimer); fitTimer = null; }
  window.removeEventListener("resize", scheduleFit);
  document.removeEventListener("keydown", pageFindShortcut, true);
  models.forEach((m) => {
    if (m.ws) { try { m.ws.close(); } catch (e) { /* already gone */ } }
    if (m.term) { try { m.term.dispose(); } catch (e) { /* already gone */ } m.term = null; }   // disposes attached addons, search included
    if (m.overlay) m.overlay.dispose();
  });
  models = [];
  active = null;
  sessions = [];
  targets = null;
}
