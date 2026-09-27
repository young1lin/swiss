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

                                                
                                     
import { iconNode } from "./ui/icon.js";
import { emptyNode } from "./ui/page.js";
/* i18n has no import back into util (its element lookup is a local byId), so this edge is
   one-way: util may localize its own copy without a module cycle. */
import { locale, tr } from "./i18n.js";
const TOKEN_ID_KEY = "swiss.tokenId"; // which token copied connect commands embed
const THEME_KEY = "swiss_theme";       // auto | light | dark — the preference, not the result
/* The three kind pages, in tab order. Typed so a kind indexes McpDetail's three slots
   directly (d[kind]) - the tab string narrows through isMcpKind, not through a cast. */
const KINDS                     = ["tools", "resources", "prompts"];
function isMcpKind(s        )               { return (KINDS                     ).includes(s); }
/* Mirrors DEFAULT_GROUP in managed.ts: the name a group list starts from — an ordinary group
   the user can rename or delete, whose only privilege is being the initial FIRST entry (the
   slot unassigned MCPs render under). The server now stores it like any other name. */
const DEFAULT_GROUP = "default";


function $                                     (id        )    { return document.getElementById(id)     ; }
function el                                       (tag   , cls         , text                )                           {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}
function esc(s         )         {
  return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c                               ];
  });
}
function now()         { return new Date().toLocaleTimeString(locale()); }

/* iconNode and emptyNode live in the UI library now (docs/46 §2.1-2.2: ui/icon.ts,
 * ui/page.ts); they are re-exported below so the import sites that name util.ts keep working.
 * New code imports them from ui/. */

/** Launch-tag glyphs (docs/29): a monochrome brand mark where one exists, the text chip
 *  otherwise. Keys are the tags tag_of produces on the server: the type for in-process and
 *  remote MCPs, the command's first word for proc ones — arbitrary words appear (node, python,
 *  echo…), so this is a whitelist and everything outside it falls back to text. The icon
 *  carries the word as its aria-label: a screen reader hears the type the chip no longer spells. */
const TYPE_ICONS                         = {
  mysql: "mysql", mariadb: "mariadb", redis: "redis", pg: "pg", postgres: "pg",
  http: "globe", https: "globe", rest: "plug",
  figma: "figma", "zai-vision": "zai",
  npx: "package", uvx: "package", docker: "docker",
  remote: "remote",
  proc: "terminal",
};
function typeTagNode(tag        )         {
  const name = TYPE_ICONS[tag];
  // A mapped tag renders its glyph (the word rides the aria-label, docs/29); anything else
  // keeps the text chip exactly as before - the tag string is a text node, not markup.
  return name ? iconNode(name, tag) : tag;
}

/** The status dot's tooltip (docs/18 V6): a colour — and the idle hollow ring above all —
 *  names no behaviour of its own, so the title says it aloud, reusing the state words the row
 *  already paints, never a synonym of our own. ONE builder for every dot in the panel: the
 *  chip text once drifted between two copies, and a dot whose title disagrees with its class
 *  is the same bug one hover wide. */
function dotTitle(word        , latencyMs                , reason         )         {
  if (word === "up") return latencyMs != null ? tr("util.nMs", { n: latencyMs }) : tr("util.text");
  // Idle is the one word that explains nothing: say what the ring means — nothing is wrong,
  // it starts when it is first needed.
  if (word === "idle") return tr("util.idleStartsFirstRequest");
  if (word === "error") return reason ? tr("util.errorReason", { reason }) : tr("util.error");
  return word || ""; // starting / stopping / reconnecting / down — the word the row already shows (docs/38 L9)
}

/** One time format for row lists: time-of-day inside the last 24h, date+time beyond it (the
 *  pure time becomes ambiguous the moment a list spans midnight). Run-history had this logic as
 *  histWhen; Traffic rows now share it instead of printing bare times on pages days old. */
function whenLabel(iso                 )         {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return String(iso);
  const day = 24 * 60 * 60 * 1000;
  return (Date.now() - d.getTime() >= day)
    ? d.toLocaleString(locale())
    : d.toLocaleTimeString(locale());
}

/** Give focus back to the control a hand (or a key) pressed, once its write has answered. Such a
 *  control is disabled while the write is out (no double send), and a browser blurs a focused
 *  control the moment it is disabled - the focus fell to <body> and the next Tab started over
 *  (found on the docs/46 P5 / P6 walks). Only when nothing else took the focus meanwhile, and
 *  only while the control is still on the page (a rebuild replaced it). */
function refocusIfIdle(pressed                )       {
  const idle = !document.activeElement || document.activeElement === document.body;
  if (pressed instanceof HTMLElement && pressed.isConnected && idle) pressed.focus();
}

// Fold state for every scope lives in groups.js now — keyed swiss.groups.<scope>.collapsed, one
// key per scope, because a group named "prod" in two lists folding together would be a
// coincidence, not a feature.

/* The toast's auto-hide timer, module-scoped (docs/37 M3): it once rode on the function
   object itself (toast._t), which needed a global Function augmentation to type. */
let toastTimer                                       = null;
function toast(msg        , isErr          )       {
  const t = $("toast");
  t.textContent = msg;
  t.className = "toast" + (isErr ? " err" : "");
  t.hidden = false;
  if (toastTimer !== null) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 3400);
}

/** The catch-side reader (docs/37 M5): every handler once read e.message off an any-typed
 *  catch variable; unknown is the honest type and this is the one narrowing it takes. */
function errText(e         )         {
  return e instanceof Error ? e.message : String(e);
}

/** True while the focused element is an input, textarea or select - the typing guards
 *  (auto-reload, keyboard shortcuts) must not fire mid-keystroke. No active element is
 *  "not typing": the ?? "" keeps the null-coercion era's answer for the null case. */
function isTyping()          {
  return /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName ?? "");
}

/** event.target narrowed to Element or null (docs/37 M3): the global EventTarget
 *  augmentation is retired, and this is the one narrowing every delegated click handler
 *  shares - the old `target && target.closest && ...` probe as a helper. The duck check,
 *  not instanceof: the suite runs under node with no DOM globals, and a Window or document
 *  target (no closest) must answer null exactly as the inline probe always did. */
function targetEl(e       )                 {
  const t = e.target                  ;
  return t && typeof t.closest === "function" ? t : null;
}


async function api(path        , opts              )                    {
  opts = opts || {};
  opts.headers = Object.assign({ "Content-Type": "application/json" }, opts.headers || {});
  const r = await fetch(path, opts);
  // docs/48: a 401 from /api is the session gate - this browser's sign-in is gone (expired,
  // or the session file was reset). The shell's own address answers with the sign-in page.
  if (r.status === 401 && typeof location !== "undefined" && path.startsWith("/api/")) {
    location.assign("/");
  }
  return r;
}

/** A response-race guard for the db loaders (the DDL sheet's S.seq pattern, factored out):
 *  a request takes a token from issue() right before it fires, and its response may only
 *  write state while accepts(token) holds — a slow answer that lands after a newer request
 *  started is dropped silently instead of overwriting what the newer one painted. Pure. */
function dbReqGuard()                                                       {
  let seq = 0;
  return {
    issue: () => { return ++seq; },
    accepts: (token) => { return token === seq; },
  };
}

/** Call the API and hand back the parsed body, or null once the failure has been reported. Every
 *  mutation repeated the same fetch → parse → toast dance, so the dance lives here once. */
async function apiJson             (path        , opts              )                    {
  try {
    const r = await api(path, opts);
    // r.json() resolves `any` by DOM typing; unknown is what a parsed body honestly is.
    const j          = await r.json().catch(()          => { return {}; });
    if (!r.ok) {
      // Failure bodies are the API's own { error } envelope - narrowed, not assumed.
      const msg = typeof j === "object" && j !== null && "error" in j && typeof j.error === "string" ? j.error : "";
      toast(msg || tr("util.httpN", { n: r.status }), true);
      return null;
    }
    // The one trust every caller grants the admin API: an ok body is the T the caller declared.
    return j     ;
  } catch (e) {
    // A network-level failure (gateway stopped mid-click) must be reported too: every caller is
    // `if (!j) return;`, and a silent null made clicking Commit do literally nothing after the
    // confirm dialog.
    toast(tr("util.requestFailedGatewayRunning"), true);
    return null;
  }
}

/* esc() survives for the few string contexts that remain (sheet titles via textContent
 * builds are nodes now; the callers left are attribute values and pure-string suites). */
/* master dropped the legacy local-storage migration helper; panel-ui keeps refocusIfIdle. */
export { $, DEFAULT_GROUP, KINDS, THEME_KEY, TOKEN_ID_KEY, api, apiJson, dbReqGuard, dotTitle, el, emptyNode, errText, esc, iconNode, isMcpKind, isTyping, now, refocusIfIdle, targetEl, toast, typeTagNode, TYPE_ICONS, whenLabel };
