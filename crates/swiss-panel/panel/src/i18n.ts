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

/* The panel's second language (docs/38). English is both the source and the key: every
   visible string is written in English where it is used and wrapped in tr()/trn(); Chinese
   is one flat dictionary fetched on demand. Nothing here probes navigator.language — the
   preference is per-browser (localStorage), exactly like the theme, because it describes
   this screen, not the user.

   Module-scope state only (no window.*): the installed language and the dictionary are two
   lets. Module top level NEVER calls tr() (docs/38 L7): imported modules evaluate before
   main.ts's await loadLocale() resolves, so a top-level tr() would freeze English onto a
   Chinese screen. */
import { $ } from "./util.js";
import { currentView } from "./ui-state.js";
import type * as PageRegistry from "./page-registry.js";

export type Lang = "en" | "zh-CN";
export const LANG_KEY = "swiss_lang"; // absent = "en"; the preference, like THEME_KEY

let lang: Lang = "en";
let dict: Record<string, string> | null = null;

/* --- preference --------------------------------------------------------------------------------- */

/** The stored preference (L4): anything absent or unrecognized answers "en" — no browser
 *  probing, an unset browser gets English, full stop. A blocked localStorage is the same as
 *  no preference. */
export function langPref(): Lang {
  try { return localStorage.getItem(LANG_KEY) === "zh-CN" ? "zh-CN" : "en"; }
  catch (e) { return "en"; }
}

/** The BCP-47 tag for Intl and toLocale* (L8): "en" | "zh-CN" for whatever is installed.
 *  Callers pass this where a bare toLocaleString() would otherwise follow the browser's
 *  language instead of the panel's choice. */
export function locale(): string { return lang; }

/* <html lang> follows the installed language everywhere it changes: browsers pick CJK
   glyph variants by it, so it is correctness, not decoration. Guarded for the node-env
   suite, which has no document. */
function syncHtmlLang(l: Lang): void {
  if (typeof document !== "undefined") document.documentElement.lang = l;
}

/** Store the choice ("en" removes the key — English is the unset default) and keep
 *  <html lang> in step. */
export function setLang(l: Lang): void {
  try {
    if (l === "en") localStorage.removeItem(LANG_KEY);
    else localStorage.setItem(LANG_KEY, l);
  } catch (e) { /* the choice still applies to this page load */ }
  syncHtmlLang(l);
}

/** The flip target of the 文/A button: en → zh-CN → en. */
export function nextLang(): Lang { return lang === "en" ? "zh-CN" : "en"; }

/* --- lookup ------------------------------------------------------------------------------------- */

/** Substitute {name} placeholders; a var the caller did not provide stays literally in the
 *  text (never swallowed, never thrown) so a missing slot is visible in the string itself. */
function apply(s: string, vars?: Record<string, string | number>): string {
  if (!vars) return s;
  return s.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (whole: string, name: string): string => {
    return Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : whole;
  });
}

/** The one lookup (L1): a missing dictionary entry returns the English key itself — the
 *  panel can never render a key name or an undefined. */
export function tr(key: string, vars?: Record<string, string | number>): string {
  return apply(dict !== null && Object.prototype.hasOwnProperty.call(dict, key) ? dict[key] : key, vars);
}

/** Plural-aware lookup (L2): Intl.PluralRules of the INSTALLED locale picks one/other, {n}
 *  is injected, and caller vars may override it. Chinese only ever selects "other", so the
 *  dictionary carries a single {n}-carrying entry per plural (L3). */
export function trn(n: number, one: string, other: string, vars?: Record<string, string | number>): string {
  const form = new Intl.PluralRules(locale()).select(n);
  return tr(form === "one" ? one : other, Object.assign({ n }, vars));
}

/** Identity (L7): marks an English string stored in a module-level table for a later tr()
 *  at paint time, so the completeness scanner sees the key while the table itself never
 *  freezes a translation at module-eval time. */
export function tk(key: string): string { return key; }

/* --- install / load ----------------------------------------------------------------------------- */

/** Swap the active language in memory. Tests and loadLocale are the callers; setLang is the
 *  preference write, install is the state write — null is English (no dictionary). */
export function install(l: Lang, table: Record<string, string> | null): void {
  lang = l;
  dict = table;
  syncHtmlLang(l);
}

/** Resolve the preference into memory at boot (and again after a flip): the Chinese
 *  dictionary is fetched only when the preference is Chinese — an English browser downloads
 *  none of it (docs/38 §1.1, the "ruthlessly small" row). */
export async function loadLocale(): Promise<void> {
  if (langPref() === "zh-CN") {
    const table: { default: Record<string, string> } = await import("./locales/zh.js");
    install("zh-CN", table.default);
  } else {
    install("en", null);
  }
}

/* --- the 文/A button and the static chrome ------------------------------------------------------- */

/* main.ts hands paintThemeBtn over at wiring time: the theme button's title is dynamic
   chrome (it names the theme clicking switches TO), so the repaint must re-run it after a
   language flip. i18n cannot import main.ts — the entry module — so the callback is the one
   edge, and it is the same single source for those two strings. */
let repaintThemeBtn: (() => void) | null = null;

/** Repaint the static chrome index.html carries in English (docs/38 §1.4): in English this
 *  writes the same strings back (a no-op); in Chinese it swaps them. The 文/A button's own
 *  title is NOT here — see paintLangBtn. */
export function paintChrome(): void {
  const filter = $<HTMLInputElement>("filter");
  filter.placeholder = tr("Search");
  filter.setAttribute("aria-label", tr("Filter MCPs"));
  const add = $("addBtn");
  add.title = tr("New group");
  add.setAttribute("aria-label", tr("New group"));
  $("themeBtn").setAttribute("aria-label", tr("Appearance"));
  const expand = $("expandBtn");
  expand.title = tr("Focus mode — hide app navigation (Esc exits)");
  expand.setAttribute("aria-label", tr("Focus mode"));
  $("langBtn").setAttribute("aria-label", tr("Language"));
  $("sideCap").textContent = tr("MCPs");
  // The loading placeholder too: the first /api/memory answer overwrites it in both
  // languages (polling.ts owns the real reading).
  $("memChip").textContent = tr("mem …");
  $("memChip").title = tr("swiss resident set");
  const rail = document.querySelector("aside.rail");
  if (rail) rail.setAttribute("aria-label", tr("Plugins"));
  $("railNav").setAttribute("aria-label", tr("Plugins"));
  $("list").setAttribute("aria-label", tr("Hosted MCPs"));
  if (repaintThemeBtn) repaintThemeBtn();
}

/** The button's title is the one string in the panel written in the TARGET language
 *  (docs/38 §2.2): on an English screen only "切换到中文" tells a Chinese reader where to
 *  click, and vice versa — a reader fluent in the current language needs no label at all.
 *  This pair deliberately stays out of the dictionary (the scanner never sees it). */
function paintLangBtn(): void {
  $("langBtn").title = lang === "en" ? "切换到中文" : "Switch to English";
}

/** One click on 文/A (L6): a canLeave veto aborts BEFORE the preference is written — an
 *  unsaved form must not cost the user their language. Then setLang → loadLocale (the
 *  dictionary download happens on the flip into Chinese) → repaint the chrome → remount the
 *  current page. No reload: it would drop form state and cut the terminal stream for a
 *  cosmetic change. page-registry loads at click time because its graph imports this module
 *  back once its labels go through tr() — a static edge would close that cycle. */
export async function toggleLang(): Promise<void> {
  const reg: typeof PageRegistry = await import("./page-registry.js");
  if (!reg.pageCanLeave()) return;
  setLang(nextLang());
  await loadLocale();
  paintChrome();
  await reg.navigatePage(currentView(), true);
  paintLangBtn();
}

/** Wire the button (called once from main.ts, which also hands over paintThemeBtn so the
 *  chrome repaint can refresh the theme title through its own tr() calls). */
export function initLangButton(onThemeRepaint?: () => void): void {
  repaintThemeBtn = onThemeRepaint ?? null;
  const b = $("langBtn");
  b.onclick = (e) => { e.stopPropagation(); void toggleLang(); };
  paintLangBtn();
}
