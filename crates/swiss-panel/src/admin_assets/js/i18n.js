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

/* The panel's second language (SPEC §panel.i18n). Every visible string is a symbolic key
   (`<module>.<semanticId>`) wrapped in tr()/trn(); English (locales/en.ts) is the always-loaded
   floor and Chinese is one flat dictionary fetched on demand. Nothing here probes navigator.language — the
   preference is per-browser (localStorage), exactly like the theme, because it describes
   this screen. Explicit choices follow across open pages through PANEL.PREFS.

   Module-scope state only (no window.*): the installed language and the dictionary are two
   lets. Module top level NEVER calls tr() (SPEC §panel.i18n): imported modules evaluate before
   main.ts's await loadLocale() resolves, so a top-level tr() would freeze English onto a
   Chinese screen. */
import { relayPrefs } from "./bus/prefs.js";
import { currentView } from "./ui-state.js";
                                                        

/* Local element lookup instead of util's $: util will import locale/tr from here for its
   own copy (dotTitle, whenLabel), and a cycle - even a benign call-time one - is not worth
   having when the helper is one line. */
function byId                                     (id        )    { return document.getElementById(id)     ; }

;                                 
export const LANG_KEY = "swiss_lang"; // absent = "en"; the preference, like THEME_KEY

import enTable from "./locales/en.js";

let lang       = "en";
let dict                                = null;
/* The last setLang() choice, kept in memory: when localStorage is blocked the stored
 * preference cannot exist, so this is the preference the loader falls back to and the
 * flip targets read (SPEC §panel.i18n's "the choice still applies to this page load"). */
let wanted       = "en";
/* English is the fallback chain's floor and every language renders it, so its table is a
 * static leaf import (no cycle: locales import nothing); each locale above it stays lazy. */
let enDict                         = enTable;

/* --- preference --------------------------------------------------------------------------------- */

/** The stored preference (L4): anything absent or unrecognized answers "en" — no browser
 *  probing, an unset browser gets English, full stop. A blocked localStorage falls back to
 *  the last in-memory choice rather than to English, so a flip still takes effect for the
 *  page load even when nothing can be persisted. */
export function langPref()       {
  try { return localStorage.getItem(LANG_KEY) === "zh-CN" ? "zh-CN" : "en"; }
  catch (e) { return wanted; }
}

/** The BCP-47 tag for Intl and toLocale* (L8): "en" | "zh-CN" for whatever is installed.
 *  Callers pass this where a bare toLocaleString() would otherwise follow the browser's
 *  language instead of the panel's choice. */
export function locale()         { return lang; }

/* <html lang> follows the installed language everywhere it changes: browsers pick CJK
   glyph variants by it, so it is correctness, not decoration. Guarded for the node-env
   suite, which has no document. */
function syncHtmlLang(l      )       {
  if (typeof document !== "undefined") document.documentElement.lang = l;
}

/** Store the choice ("en" removes the key — English is the unset default) and keep
 *  <html lang> in step. */
export function setLang(l      )       {
  wanted = l;
  try {
    if (l === "en") localStorage.removeItem(LANG_KEY);
    else localStorage.setItem(LANG_KEY, l);
  } catch (e) { /* the choice still applies to this page load */ }
  syncHtmlLang(l);
}

/** The flip target of the 文/A button: en → zh-CN → en. Based on the CHOICE, not the
 *  installed state: while a first zh dictionary download is still in flight the choice is
 *  already zh-CN, so a second click correctly targets English — two rapid clicks land on
 *  English, they do not get swallowed as one flip. */
export function nextLang()       { return langPref() === "en" ? "zh-CN" : "en"; }

/* --- lookup ------------------------------------------------------------------------------------- */

/** Substitute {name} placeholders; a var the caller did not provide stays literally in the
 *  text (never swallowed, never thrown) so a missing slot is visible in the string itself. */
function apply(s        , vars                                  )         {
  if (!vars) return s;
  return s.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (whole        , name        )         => {
    return Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : whole;
  });
}

/** The one lookup (L1): a missing dictionary entry falls back to the English table, and
 *  a key carried by neither table renders as the key itself — visible in the string, never
 *  undefined, never swallowed. The completeness gate keeps the last state unreachable for
 *  literal keys; dynamically derived keys must stay within table bounds by construction. */
export function tr(key        , vars                                  )         {
  const hit = dict !== null && Object.prototype.hasOwnProperty.call(dict, key) ? dict[key]
    : Object.prototype.hasOwnProperty.call(enDict, key) ? enDict[key]
    : key;
  return apply(hit, vars);
}

/** Plural-aware lookup (L2): Intl.PluralRules of the INSTALLED locale picks one/other, {n}
 *  is injected, and caller vars may override it. Chinese only ever selects "other", so the
 *  dictionary carries a single {n}-carrying entry per plural (L3). */
export function trn(n        , one        , other        , vars                                  )         {
  const form = new Intl.PluralRules(locale()).select(n);
  return tr(form === "one" ? one : other, Object.assign({ n }, vars));
}

/** Identity (L7): marks an English string stored in a module-level table for a later tr()
 *  at paint time, so the completeness scanner sees the key while the table itself never
 *  freezes a translation at module-eval time. */
export function tk(key        )         { return key; }

/** The wire vocabulary (L9's one carve-out): the gateway serves nav group/page labels as
 *  English TEXT on /api/plugins, not as keys, so the panel maps served text -> wire.* key
 *  here. An unknown label (a plugin added after this build) passes through and renders as
 *  served. The wire test in i18n-complete.test.ts pins this list against the tables. */
const WIRE_LABELS                         = {
  "MCP": "wire.mcp", "Tunnels": "wire.tunnels", "Data": "wire.data", "Jobs": "wire.jobs",
  "Process": "wire.process", "Terminal": "wire.terminal", "Remote": "wire.remote",
  "Settings": "wire.settings", "Servers": "wire.servers", "Traffic": "wire.traffic",
  "Token": "wire.token", "SSH Connections": "wire.sshConnections",
  "Port Forwards": "wire.portForwards", "Targets": "wire.targets", "Runs": "wire.runs",
  "Plugins": "wire.plugins", "Secrets": "wire.secrets", "System": "wire.system",
  "Memory": "wire.memory", "LLM": "wire.llm", "Providers": "wire.providers",
  "Embedding": "wire.embedding", "Agents": "wire.agents", "History": "wire.history",
};

/** Map a gateway-served label to its key; unknown text passes through unchanged. */
export function wireLabel(served        )         {
  return Object.prototype.hasOwnProperty.call(WIRE_LABELS, served) ? WIRE_LABELS[served] : served;
}

/** Gateway refusals by their wire code (SPEC §panel.i18n): a route that refuses answers
 *  `{error, code, params?}`, the English sentence for the CLI and the logs and a stable code
 *  this table turns into the reader's language. A code added on the server lands here, in
 *  both locales, in the same change. */
const SERVED_CODES                         = {
  "llm.tools.runtime": tk("served.llmToolsRuntime"),
  "llm.tools.tool": tk("served.llmToolsTool"),
  "llm.tools.directory": tk("served.llmToolsDirectory"),
  "llm.context.conversation": tk("served.llmContextConversation"),
  "console.exists": tk("served.consoleExists"),
  "console.removed": tk("served.consoleRemoved"),
  "console.changed": tk("served.consoleChanged"),
  "console.changedDelete": tk("served.consoleChangedDelete"),
  "console.tooLarge": tk("served.consoleTooLarge"),
  "console.connection": tk("served.consoleConnection"),
  "bus.unavailable": tk("served.busUnavailable"),
  "bus.unknown": tk("served.busUnknown"),
  "bus.badBody": tk("served.busBadBody"),
  "bus.notRelayable": tk("served.busNotRelayable"),
  "bus.limit": tk("served.busLimit"),
  "bus.offline": tk("served.busOffline"),
  "bus.timeout": tk("served.busTimeout"),
  "bus.tooLarge": tk("served.busTooLarge"),
};

/** A refusal's words: its code's own copy, filled from `params`; a code this build does not
 *  know yet, or a body with none (an engine error), shows the server's `error` as sent, and
 *  a body without even that, `fallback`. */
export function servedError(body         , fallback        )         {
  const b = body && typeof body === "object" ? body                                                          : {};
  const key = typeof b.code === "string" && Object.prototype.hasOwnProperty.call(SERVED_CODES, b.code) ? SERVED_CODES[b.code] : null;
  if (key) {
    const vars                                  = {};
    if (b.params && typeof b.params === "object") {
      for (const [k, v] of Object.entries(b.params)) if (typeof v === "string" || typeof v === "number") vars[k] = v;
    }
    return tr(key, vars);
  }
  return typeof b.error === "string" && b.error ? b.error : fallback;
}

/** A bus fault uses the same code table as an HTTP refusal (SPEC §bus.wire, SPEC §panel.i18n). */
export function servedFault(fault                                                                                            )         {
  if (!fault) return "";
  return servedError({ error: fault.message, code: fault.code, params: fault.params }, fault.message || "");
}

const QUERY_ONLY_HEAD = "this connection is query-only (";
const QUERY_ONLY_TAIL = "): a write would change this module's own database";
const READ_HEAD = "only a read runs here (SELECT, WITH, TABLE, VALUES, SHOW, DESCRIBE, EXPLAIN), not ";
const READ_OUTFILE = "only a read runs here: INTO OUTFILE and INTO DUMPFILE write a server file";
const ANALYZE_WRITES = "ANALYZE writes sqlite_stat1";
const NOT_THERE_HEAD = "this module database is not there yet (";
const NO_SESSIONS = "this connection is query-only and has no sessions to cancel";
const IMPORT_WRITES = "sqlite is query-only — an import would write the built-in memory store";

/** The module-database refusals the gateway writes in English (SPEC §data.sqlite). A known
 *  sentence is the panel's own copy; an engine error, or any other sentence, passes through. */
export function moduleDbMessage(served        )         {
  if (served === NO_SESSIONS) return tr("dataSqlite.noSessions");
  if (served === IMPORT_WRITES) return tr("dataSqlite.importWrites");
  if (served.startsWith(NOT_THERE_HEAD) && served.endsWith(")")) {
    return tr("dataSqlite.notThere", { label: served.slice(NOT_THERE_HEAD.length, -1) });
  }
  if (served.startsWith(QUERY_ONLY_HEAD)) {
    const tailAt = served.indexOf(QUERY_ONLY_TAIL);
    if (tailAt > QUERY_ONLY_HEAD.length) {
      const label = served.slice(QUERY_ONLY_HEAD.length, tailAt);
      const rest = served.slice(tailAt + QUERY_ONLY_TAIL.length);
      if (rest === "") return tr("dataSqlite.queryOnly", { label });
      if (rest.startsWith(" (") && rest.endsWith(")")) {
        const raw = rest.slice(2, -1);
        return tr("dataSqlite.queryOnlyReason", { label, reason: readReason(raw) ?? raw });
      }
    }
  }
  return readReason(served) ?? served;
}

function readReason(reason        )                {
  if (reason === READ_OUTFILE) return tr("dataSqlite.readOutfile");
  if (reason === ANALYZE_WRITES) return tr("dataSqlite.analyzeWrites");
  if (!reason.startsWith(READ_HEAD)) return null;
  const statement = reason.slice(READ_HEAD.length);
  if (!statement) return null;
  return tr("dataSqlite.readOnly", {
    statement: statement === "an empty statement" ? tr("dataSqlite.emptyStatement") : statement,
  });
}

/* --- install / load ----------------------------------------------------------------------------- */

/** Swap the active language in memory. Tests and loadLocale are the callers; setLang is the
 *  preference write, install is the state write — null is English (no dictionary). */
export function install(l      , table                               )       {
  lang = l;
  dict = table;
  syncHtmlLang(l);
}

/** Test seam: hand the chain a substitute English table (the shipped one loads with the
 *  module; only the vitest suite ever calls this). */
export function installEnglish(table                        )       { enDict = table; }

/** Resolve the preference into memory at boot (and again after a flip): the Chinese
 *  dictionary is fetched only when the preference is Chinese — an English browser downloads
 *  none of it (SPEC §panel.i18n, the "ruthlessly small" row). */
export async function loadLocale()                {
  if (langPref() === "zh-CN") {
    try {
      const table                                      = await import("./locales/zh.js");
      /* A flip back to English may have landed while this download was in flight (the
       * module var, not storage, is the tiebreaker when storage is blocked): installing a
       * dictionary the user has already clicked away from would be a lost update. */
      if (langPref() === "zh-CN") install("zh-CN", table.default);
    } catch (e) {
      /* The dictionary failed to arrive (a stale-embed build serving an old asset tree is
       * the realistic way): fall back to the English floor instead of rejecting — boot's
       * top-level await would otherwise take the whole panel down with it. */
      console.warn("zh dictionary unavailable, staying on English", e);
      install("en", null);
    }
  } else {
    install("en", null);
  }
}

/* --- the 文/A button and the static chrome ------------------------------------------------------- */

/* main.ts hands paintThemeBtn over at wiring time: the theme button's title is dynamic
   chrome (it names the theme clicking switches TO), so the repaint must re-run it after a
   language flip. i18n cannot import main.ts — the entry module — so the callback is the one
   edge, and it is the same single source for those two strings. */
let repaintThemeBtn                      = null;
let repaintBusLine                      = null;

/** The reconnecting line is chrome too (SPEC §bus.link). main.ts owns the node. */
export function setBusLinePainter(fn            )       { repaintBusLine = fn; }

/** Repaint the static chrome index.html carries in English (SPEC §panel.i18n): in English this
 *  writes the same strings back (a no-op); in Chinese it swaps them. The 文/A button's own
 *  title is NOT here — see paintLangBtn. */
export function paintChrome()       {
  const filter = byId                  ("filter");
  filter.placeholder = tr("i18n.search");
  filter.setAttribute("aria-label", tr("i18n.filterMcps"));
  const add = byId("addBtn");
  add.title = tr("i18n.newGroup");
  add.setAttribute("aria-label", tr("i18n.newGroup"));
  byId("themeBtn").setAttribute("aria-label", tr("i18n.appearance"));
  const expand = byId("expandBtn");
  expand.title = tr("i18n.focusModeHideApp");
  expand.setAttribute("aria-label", tr("i18n.focusMode"));
  byId("langBtn").setAttribute("aria-label", tr("i18n.language"));
  byId("sideCap").textContent = tr("i18n.mcps");
  // The loading placeholder too: the first /api/memory answer overwrites it in both
  // languages (polling.ts owns the real reading).
  byId("memChip").textContent = tr("i18n.mem");
  byId("memChip").title = tr("i18n.swissResidentSet");
  const rail = document.querySelector("aside.rail");
  if (rail) rail.setAttribute("aria-label", tr("i18n.plugins"));
  byId("railNav").setAttribute("aria-label", tr("i18n.plugins"));
  byId("list").setAttribute("aria-label", tr("i18n.hostedMcps"));
  // Not index.html's, but chrome all the same: the shell builds it once at boot (pane-scroll.ts).
  const top = document.getElementById("toTop");
  if (top) {
    top.title = tr("ui.toTop");
    top.setAttribute("aria-label", tr("ui.toTop"));
  }
  if (repaintThemeBtn) repaintThemeBtn();
  if (repaintBusLine) repaintBusLine();
}

/** The button's title is the one string in the panel written in the TARGET language
 *  (SPEC §panel.i18n): on an English screen only "切换到中文" tells a Chinese reader where to
 *  click, and vice versa — a reader fluent in the current language needs no label at all.
 *  This pair deliberately stays out of the dictionary (the scanner never sees it). */
function paintLangBtn()       {
  byId("langBtn").title = lang === "en" ? "切换到中文" : "Switch to English";
}

/** One click on 文/A (L6): a canLeave veto aborts BEFORE the preference is written — an
 *  unsaved form must not cost the user their language. Then setLang → loadLocale (the
 *  dictionary download happens on the flip into Chinese) → repaint the chrome → remount the
 *  current page. No reload: it would drop form state and cut the terminal stream for a
 *  cosmetic change. page-registry loads at click time because its graph imports this module
 *  back once its labels go through tr() — a static edge would close that cycle. */
export function toggleLang()                { return changeLang(null, true); }

/** Apply a received choice through the same guarded repaint, without echo (SPEC §panel.i18n).
 *  The language already on screen and stored changes nothing, so it neither repaints nor
 *  remounts: a remount would drop the page's scroll and form state for no visible change. A
 *  flip still in flight has stored its target, so a choice that undoes it goes through. */
export function applyLang(choice      )                {
  if (choice === lang && langPref() === choice) return Promise.resolve();
  return changeLang(choice, false);
}

// Only the latest asynchronous choice may announce itself (SPEC §panel.i18n).
let langChange = 0;
async function changeLang(choice             , relay         )                {
  const reg                      = await import("./page-registry.js");
  if (reg.pageHasPendingChanges()) {
    // The remount at the end of this function would drop those changes, so the flip refuses.
    // It used to refuse in silence, which reads as a broken button: found live during a
    // second-language pass (SPEC §data.tabs), where one buffered cell edit on any Data tab made 文/A
    // answer every click with nothing. util is imported here rather than at the top for the
    // same reason page-registry is: a static edge would close a cycle back into this module.
    const { toast } = await import("./util.js");
    toast(tr("i18n.pendingBlocksSwitch"), true);
    return;
  }
  const generation = ++langChange;
  const target = choice ?? nextLang();
  setLang(target);
  await loadLocale();
  if (generation !== langChange) return;
  /* The zh download failed (loadLocale stayed on English): revert the preference so it
   * stops disagreeing with the screen every boot — the flip becomes a visible no-op that
   * the next click can retry once the asset tree is consistent again. */
  if (langPref() === "zh-CN" && lang === "en") setLang("en");
  paintChrome();
  await reg.navigatePage(currentView(), true);
  paintLangBtn();
  if (relay && generation === langChange && langPref() === target && lang === target) relayPrefs({ lang: target });
}

/** Wire the button (called once from main.ts, which also hands over paintThemeBtn so the
 *  chrome repaint can refresh the theme title through its own tr() calls). */
export function initLangButton(onThemeRepaint             )       {
  repaintThemeBtn = onThemeRepaint ?? null;
  const b = byId("langBtn");
  b.onclick = (e) => { e.stopPropagation(); void toggleLang(); };
  paintLangBtn();
}
