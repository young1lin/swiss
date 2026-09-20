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
import type { McpKind } from "./types/state.js";
import type { HChild } from "./h.js";
import { h } from "./h.js";
/* i18n has no import back into util (its element lookup is a local byId), so this edge is
   one-way: util may localize its own copy without a module cycle. */
import { locale, tr } from "./i18n.js";
const TOKEN_ID_KEY = "mcp_gateway_token_id"; // which token copied connect commands embed
const THEME_KEY = "swiss_theme";       // auto | light | dark — the preference, not the result
/* The three kind pages, in tab order. Typed so a kind indexes McpDetail's three slots
   directly (d[kind]) - the tab string narrows through isMcpKind, not through a cast. */
const KINDS: readonly McpKind[] = ["tools", "resources", "prompts"];
function isMcpKind(s: string): s is McpKind { return (KINDS as readonly string[]).includes(s); }
/* Mirrors DEFAULT_GROUP in managed.ts: the name a group list starts from — an ordinary group
   the user can rename or delete, whose only privilege is being the initial FIRST entry (the
   slot unassigned MCPs render under). The server now stores it like any other name. */
const DEFAULT_GROUP = "default";


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
function now(): string { return new Date().toLocaleTimeString(locale()); }

/* The sprite reference is a NODE (docs/18 V2, docs/37 R5): the pre-R5 string twin icon()
 * retired with the last innerHTML caller — every glyph is iconNode() now. */

/** The same sprite reference as a NODE, for the h() callers (docs/37 R5). SVG is its own
 *  namespace: createElement("svg") builds an HTMLUnknownElement that renders nothing, so this
 *  goes through createElementNS and cannot be folded into h(), which is typed for HTML tags.
 *  <use href> without the xlink alias is SVG 2 — the same markup icon() has always emitted, so
 *  the two builders agree on what reaches the browser. */
function iconNode(name: string, label?: string): SVGSVGElement {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("class", "ic");
  if (label) { svg.setAttribute("role", "img"); svg.setAttribute("aria-label", label); }
  else svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS(NS, "use");
  use.setAttribute("href", "#i-" + name);
  svg.appendChild(use);
  return svg;
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
function typeTagNode(tag: string): HChild {
  const name = TYPE_ICONS[tag];
  // A mapped tag renders its glyph (the word rides the aria-label, docs/29); anything else
  // keeps the text chip exactly as before - the tag string is a text node, not markup.
  return name ? iconNode(name, tag) : tag;
}

/** One empty state (docs/18 V7): icon, title, one line of hint, optional ghost action. Every
 *  view's "nothing here" is this shape — the Terminal alone keeps its own, because it lives in
 *  the black frame with its own tokens. The action button carries data-empty-action so the
 *  owning view can wire it without inventing per-view ids.
 *
 *  A NODE since docs/37 R5 — same markup, same data-empty-action contract; the esc() calls
 *  are gone because the title and hint are text nodes. */
function emptyNode(opts: EmptyStateSpec): HTMLElement {
  return h("div", { class: "empty" },
    h("div", null,
      h("span", { class: "empty-ic" }, iconNode(opts.icon)),
      h("h2", null, opts.title),
      opts.hint ? h("p", { class: "hint" }, opts.hint) : null,
      opts.action ? h("button", { class: "btn ghost", data: { "empty-action": opts.action } }, opts.action) : null));
}

/** The status dot's tooltip (docs/18 V6): a colour — and the idle hollow ring above all —
 *  names no behaviour of its own, so the title says it aloud, reusing the state words the row
 *  already paints, never a synonym of our own. ONE builder for every dot in the panel: the
 *  chip text once drifted between two copies, and a dot whose title disagrees with its class
 *  is the same bug one hover wide. */
function dotTitle(word: string, latencyMs?: number | null, reason?: string): string {
  if (word === "up") return latencyMs != null ? tr("up · {n} ms", { n: latencyMs }) : tr("up");
  // Idle is the one word that explains nothing: say what the ring means — nothing is wrong,
  // it starts when it is first needed.
  if (word === "idle") return tr("idle — starts on first request");
  if (word === "error") return reason ? tr("error: {reason}", { reason }) : tr("error");
  return word || ""; // starting / stopping / reconnecting / down — the word the row already shows (docs/38 L9)
}

/** One time format for row lists: time-of-day inside the last 24h, date+time beyond it (the
 *  pure time becomes ambiguous the moment a list spans midnight). Run-history had this logic as
 *  histWhen; Traffic rows now share it instead of printing bare times on pages days old. */
function whenLabel(iso: string | number): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return String(iso);
  const day = 24 * 60 * 60 * 1000;
  return (Date.now() - d.getTime() >= day)
    ? d.toLocaleString(locale())
    : d.toLocaleTimeString(locale());
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
    // r.json() resolves `any` by DOM typing; unknown is what a parsed body honestly is.
    const j: unknown = await r.json().catch((): unknown => { return {}; });
    if (!r.ok) {
      // Failure bodies are the API's own { error } envelope - narrowed, not assumed.
      const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : "";
      toast(msg || tr("HTTP {n}", { n: r.status }), true);
      return null;
    }
    // The one trust every caller grants the admin API: an ok body is the T the caller declared.
    return j as T;
  } catch (e) {
    // A network-level failure (gateway stopped mid-click) must be reported too: every caller is
    // `if (!j) return;`, and a silent null made clicking Commit do literally nothing after the
    // confirm dialog.
    toast(tr("request failed — is the gateway running?"), true);
    return null;
  }
}

/* esc() survives for the few string contexts that remain (sheet titles via textContent
 * builds are nodes now; the callers left are attribute values and pure-string suites). */
export { $, DEFAULT_GROUP, KINDS, THEME_KEY, TOKEN_ID_KEY, api, apiJson, dbReqGuard, dotTitle, el, emptyNode, errText, esc, iconNode, isMcpKind, isTyping, now, targetEl, toast, typeTagNode, TYPE_ICONS, whenLabel };
