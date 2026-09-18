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

import type { EmptyStateSpec } from "./types/dom.js";
import type { PanelState } from "./types/state.js";
const TOKEN_ID_KEY = "mcp_gateway_token_id"; // which token copied connect commands embed
const THEME_KEY = "swiss_theme";       // auto | light | dark — the preference, not the result
const KINDS = ["tools", "resources", "prompts"];
/* Mirrors DEFAULT_GROUP in managed.ts: the name a group list starts from — an ordinary group
   the user can rename or delete, whose only privilege is being the initial FIRST entry (the
   slot unassigned MCPs render under). The server now stores it like any other name. */
const DEFAULT_GROUP = "default";

const state: PanelState = {
  mcps: [],          // rows from /api/mcps (each carries .group)
  groups: [],        // group names in sidebar order — the FIRST entry is the sink slot for unassigned rows
  collapsed: {},     // mcps fold map; group name -> true. Panel-only, so it lives in localStorage (groups.js)
  selected: null,    // selected MCP name
  filter: "",
  detail: null,      // { name, tab, config, source, editing, editType, tools:{...}, calls, ... }
  busy: {},          // name -> verb in flight
  lastAction: {},    // name -> { msg, err, at }
  mem: null,
  menuOpen: false,
  info: null,        // { tokenEnv } from /api/info — the env var name
  traffic: [],       // ONE PAGE of interactions from /api/traffic — no .body/.response on a row
  trafficClients: [],       // every client in the ring, folded server-side (not just this page)
  trafficFilter: "actions", // "actions" (tools/call, resources/read, …) or "all" (incl. protocol)
  trafficClient: null,      // a clientKey to narrow the activity log, or null
  trafficPage: 0,           // 0-based, newest first
  trafficMore: false,       // is there an older page?
  trafficTotal: 0,          // rows matching the current filter, across all pages
  trafficAll: 0,            // rows in the ring before filtering — the "n of m" readout
  trafficOpen: {},          // seq -> expanded: shows the raw request JSON for that interaction
  trafficFull: {},          // seq -> { body, response }, fetched on first expand (cf. d.callsFull)
  trafficSig: null,         // data signature; a poll skips re-render when unchanged (keeps expansion)
  view: "mcps",      // "mcps" | "tunnels" | "traffic" | "data" | "jobs" — the toolbar switcher
  panelVersion: null, // admin.html mtime stamp from /api/info; a change means a new build landed
  addGroup: null,     // sidebar group a header "+" targets for the next created MCP (add-sheet.js)
  draggingGroup: null, // name of the GROUP HEADER being dragged — polls must not rebuild under it either
  db: null,           // the whole Data view state (data-view.js builds/owns it; redisValue rides on it)
  tun: {             // tunnel view state; `data` is the last /api/tunnels response
    tab: "conns",    // "conns" | "rules"
    data: null,
    busy: {},        // id -> verb in flight
    keys: null,      // cached /api/tunnels/keys
    dragging: null,  // id of the row being dragged — polls must not rebuild under it
      draggingGroup: null, // name of the group header being dragged — same rebuild freeze as a row drag
    pendingGroup: null, // group chosen via a header "+", preselected in the sheet it opens
  },
  jobs: {             // jobs view state (jobs.js renders it; polling.js loads it)
    data: [],        // rows from /api/jobs
    groups: ["default"], // the scope's group names, from /api/jobs' top level (docs/20 G4)
    collapsed: {},   // jobs fold map; group name -> true (localStorage, groups.js)
    busy: {},        // name -> Run now in flight
    painted: "",     // joined row names at last render — the poll's structural signature
    hist: null,      // the open history sheet: { name, runs }
    dragging: null,      // id of the row being dragged — polls must not rebuild under it
    draggingGroup: null, // name of the group header being dragged — same freeze as a row drag
    pendingGroup: null,  // group chosen via a header "+", preselected in the sheet it opens
  },
};

function $<T extends HTMLElement = HTMLElement>(id: string): T { return document.getElementById(id) as T; }
function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string | null): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}
function esc(s: unknown): string {
  return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c as "&" | "<" | ">" | '"' | "'"];
  });
}
function now(): string { return new Date().toLocaleTimeString(); }

/** One inline icon from the shell's sprite (docs/18 V2). Stroke follows currentColor and .ic
 *  sizes it; decorative by default, an image with a name when `label` is passed (icon buttons). */
function icon(name: string, label?: string): string {
  return label
    ? '<svg class="ic" role="img" aria-label="' + esc(label) + '"><use href="#i-' + name + '"></use></svg>'
    : '<svg class="ic" aria-hidden="true"><use href="#i-' + name + '"></use></svg>';
}

/** Launch-tag glyphs (docs/29): a monochrome brand mark where one exists, the text chip
 *  otherwise. Keys are the tags tag_of produces on the server: the type for in-process and
 *  remote MCPs, the command's first word for proc ones — arbitrary words appear (node, python,
 *  echo…), so this is a whitelist and everything outside it falls back to text. The icon
 *  carries the word as its aria-label: a screen reader hears the type the chip no longer spells. */
const TYPE_ICONS: Record<string, string> = {
  mysql: "mysql", mariadb: "mariadb", redis: "redis", pg: "pg", postgres: "pg",
  http: "globe", https: "globe", rest: "plug",
  figma: "figma", "zai-vision": "zai",
  npx: "package", uvx: "package", docker: "docker",
  remote: "remote",
  proc: "terminal",
};
function typeTagHtml(tag: string): string {
  const name = TYPE_ICONS[tag];
  if (!name) return esc(tag);
  return icon(name, tag);
}

/** One empty state (docs/18 V7): icon, title, one line of hint, optional ghost action. Every
 *  view's "nothing here" is this shape — the Terminal alone keeps its own, because it lives in
 *  the black frame with its own tokens. The action button carries data-empty-action so the
 *  owning view can wire it without inventing per-view ids. */
function emptyHtml(opts: EmptyStateSpec): string {
  const action = opts.action
    ? '<button class="btn ghost" data-empty-action="' + esc(opts.action) + '">' + esc(opts.action) + "</button>"
    : "";
  return '<div class="empty"><div>' +
    '<span class="empty-ic">' + icon(opts.icon) + "</span>" +
    "<h2>" + esc(opts.title) + "</h2>" +
    (opts.hint ? '<p class="hint">' + esc(opts.hint) + "</p>" : "") +
    action +
    "</div></div>";
}

/** The status dot's tooltip (docs/18 V6): a colour — and the idle hollow ring above all —
 *  names no behaviour of its own, so the title says it aloud, reusing the state words the row
 *  already paints, never a synonym of our own. ONE builder for every dot in the panel: the
 *  chip text once drifted between two copies, and a dot whose title disagrees with its class
 *  is the same bug one hover wide. */
function dotTitle(word: string, latencyMs?: number | null, reason?: string): string {
  if (word === "up") return latencyMs != null ? "up · " + latencyMs + " ms" : "up";
  // Idle is the one word that explains nothing: say what the ring means — nothing is wrong,
  // it starts when it is first needed.
  if (word === "idle") return "idle — starts on first request";
  if (word === "error") return reason ? "error: " + reason : "error";
  return word || ""; // starting / stopping / reconnecting / down — the word the row already shows
}

/** One time format for row lists: time-of-day inside the last 24h, date+time beyond it (the
 *  pure time becomes ambiguous the moment a list spans midnight). Run-history had this logic as
 *  histWhen; Traffic rows now share it instead of printing bare times on pages days old. */
function whenLabel(iso: string | number): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return String(iso);
  const day = 24 * 60 * 60 * 1000;
  return (Date.now() - d.getTime() >= day)
    ? d.toLocaleString()
    : d.toLocaleTimeString();
}

// Fold state for every scope lives in groups.js now — keyed swiss.groups.<scope>.collapsed, one
// key per scope, because a group named "prod" in two lists folding together would be a
// coincidence, not a feature.

/* The toast's auto-hide timer, module-scoped (docs/37 M3): it once rode on the function
   object itself (toast._t), which needed a global Function augmentation to type. */
let toastTimer: ReturnType<typeof setTimeout> | null = null;
function toast(msg: string, isErr?: boolean): void {
  const t = $("toast");
  t.textContent = msg;
  t.className = "toast" + (isErr ? " err" : "");
  t.hidden = false;
  if (toastTimer !== null) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 3400);
}

/** The catch-side reader (docs/37 M5): every handler once read e.message off an any-typed
 *  catch variable; unknown is the honest type and this is the one narrowing it takes. */
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** True while the focused element is an input, textarea or select - the typing guards
 *  (auto-reload, keyboard shortcuts) must not fire mid-keystroke. No active element is
 *  "not typing": the ?? "" keeps the null-coercion era's answer for the null case. */
function isTyping(): boolean {
  return /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName ?? "");
}

/** event.target narrowed to Element or null (docs/37 M3): the global EventTarget
 *  augmentation is retired, and this is the one narrowing every delegated click handler
 *  shares - the old `target && target.closest && ...` probe as a helper. The duck check,
 *  not instanceof: the suite runs under node with no DOM globals, and a Window or document
 *  target (no closest) must answer null exactly as the inline probe always did. */
function targetEl(e: Event): Element | null {
  const t = e.target as Element | null;
  return t && typeof t.closest === "function" ? t : null;
}


async function api(path: string, opts?: RequestInit): Promise<Response> {
  opts = opts || {};
  opts.headers = Object.assign({ "Content-Type": "application/json" }, opts.headers || {});
  return fetch(path, opts);
}

/** A response-race guard for the db loaders (the DDL sheet's S.seq pattern, factored out):
 *  a request takes a token from issue() right before it fires, and its response may only
 *  write state while accepts(token) holds — a slow answer that lands after a newer request
 *  started is dropped silently instead of overwriting what the newer one painted. Pure. */
function dbReqGuard(): { issue(): number; accepts(token: number): boolean } {
  let seq = 0;
  return {
    issue: () => { return ++seq; },
    accepts: (token) => { return token === seq; },
  };
}

/** Call the API and hand back the parsed body, or null once the failure has been reported. Every
 *  mutation repeated the same fetch → parse → toast dance, so the dance lives here once. */
async function apiJson<T = unknown>(path: string, opts?: RequestInit): Promise<T | null> {
  try {
    const r = await api(path, opts);
    const j: { error?: string } = await r.json().catch(() => { return {}; });
    if (!r.ok) { toast(j.error || "HTTP " + r.status, true); return null; }
    return j as unknown as T;
  } catch (e) {
    // A network-level failure (gateway stopped mid-click) must be reported too: every caller is
    // `if (!j) return;`, and a silent null made clicking Commit do literally nothing after the
    // confirm dialog.
    toast("request failed — is the gateway running?", true);
    return null;
  }
}

export { $, DEFAULT_GROUP, KINDS, THEME_KEY, TOKEN_ID_KEY, api, apiJson, dbReqGuard, dotTitle, el, emptyHtml, errText, esc, icon, isTyping, now, state, targetEl, toast, typeTagHtml, TYPE_ICONS, whenLabel };
