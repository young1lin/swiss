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
   Rendering contract, and the reason this panel is not one big innerHTML template any more:
   the old panel rebuilt the whole table from a string every 6 seconds, which flickered, dropped text
   selection and scroll position, and — a real bug — wiped whatever you had typed into the edit form.

   So: the 6s poll may ONLY patch the sidebar rows and the detail header (dot, subtitle, primary
   action). The tab body is rebuilt only in response to an explicit load or user action, never by the
   poll, and never while it holds focus.

   This is the entry module: boot, appearance, the toolbar/keyboard wiring, and the poll. Every view
   lives in its own module next to this one; /admin/js/* is served with no-store, so editing any of
   them reaches the browser on the next reload — no build, no gateway restart.
   ================================================================================================ */
import { $, THEME_KEY, api, state, toast } from "./util.js";
import { loadCollapsed } from "./groups.js";
import { closeSheet } from "./add-sheet.js";
import { initSelects } from "./dropdown.js";
import { initPages, pageHasPendingChanges, pageUsesSidebar, pollPage } from "./page-registry.js";
import { openDetail } from "./detail.js";
import { patchSidebar } from "./menu.js";
import { closeMenu } from "./pane.js";
import { loadMemory, refreshNow } from "./polling.js";
import { exitImmersive, immersiveOn, initImmersive } from "./immersive.js";
import { histClose } from "./run-history.js";
import { navRows, nudgeSelected } from "./sidebar.js";


/** A new panel build has landed. Reload in place — the same tab, never a new one — but only
 *  when the reload cannot destroy work: no buffered data-view edits, no open sheet, nothing
 *  being typed. Otherwise say so once and keep checking on later polls. */
function maybeReloadPanel(newVersion: string): void {
  var typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement && document.activeElement.tagName);
  var busy = !$("sheet").hidden || state.menuOpen;
  var edits = pageHasPendingChanges();
  if (typing || busy || edits) {
    if (!maybeReloadPanel._warned) {
      maybeReloadPanel._warned = true;
      toast("A new panel version is ready — it will load once you finish editing");
    }
    return;
  }
  state.panelVersion = newVersion; // remember before reload so a fast retry does not loop
  location.reload();
}

/* --- boot -------------------------------------------------------------------------------------- */
/** The panel has no login: the gateway only ever accepts loopback requests, so reaching this page
 *  at all already means you are on the machine it serves. Nothing to sign in to — just start. */
function showApp(): void {
  void initPages();
  loadInfo();
}

/** Host facts the panel needs once: the token's env var name, and the named-token list (no
 *  secrets) for the management view. */
async function loadInfo(): Promise<void> {
  try {
    var r = await api("/api/info");
    if (r.ok) {
      state.info = await r.json();
      // First sight records the stamp; a LATER, DIFFERENT one means the panel was rebuilt —
      // reload in place when nothing unsaved would be lost (checked in maybeReloadPanel).
      if (state.panelVersion === null) state.panelVersion = state.info!.panelVersion || null;
      else if (state.info!.panelVersion && state.info!.panelVersion !== state.panelVersion) {
        maybeReloadPanel(state.info!.panelVersion!);
      }
    }
    var t = await api("/api/tokens");
    if (t.ok) state.tokens = (await t.json()).tokens || [];
  } catch (e) { /* the token list just stays empty */ }
}
/* --- appearance ------------------------------------------------------------------------------- */
/* Three choices, not a two-state switch: "auto" has to stay reachable, because on a machine that
   turns dark at sunset the right answer changes twice a day and a toggle can only ever be wrong
   half of it. The preference is per-browser (localStorage), not gateway state — it describes this
   screen, and the same gateway is read from other screens with other lighting. */
function themePref(): "light" | "dark" | "auto" {
  try { var v = localStorage.getItem(THEME_KEY); return v === "light" || v === "dark" ? v : "auto"; }
  catch (e) { return "auto"; }
}
function prefersDark(): boolean {
  return !!(window.matchMedia as unknown && matchMedia("(prefers-color-scheme: dark)").matches);
}
function applyTheme(): void {
  var p = themePref();
  document.documentElement.setAttribute("data-theme", p === "dark" || (p === "auto" && prefersDark()) ? "dark" : "light");
}
function setTheme(p: string): void {
  try { if (p === "auto") localStorage.removeItem(THEME_KEY); else localStorage.setItem(THEME_KEY, p); }
  catch (e) { /* the choice still applies to this page load */ }
  applyTheme();
}
/* One click, one flip: the button shows what clicking switches TO — a moon on the light theme
   (click → dark), a sun on the dark one (click → light). "Auto" stays the untouched default (no
   stored preference): the first explicit click pins the choice for this browser. */
function paintThemeBtn(): void {
  var dark = document.documentElement.getAttribute("data-theme") === "dark";
  var b = $("themeBtn");
  // The sprite swap (docs/18 V2): switch the referenced symbol, not the button's HTML.
  var use = b.querySelector("use");
  if (use) use.setAttribute("href", dark ? "#i-sun" : "#i-moon");
  b.title = dark ? "Switch to light" : "Switch to dark";
}
$("themeBtn").onclick = function (e) {
  e.stopPropagation();
  var dark = document.documentElement.getAttribute("data-theme") === "dark";
  setTheme(dark ? "light" : "dark");
  paintThemeBtn();
};
paintThemeBtn();
// Every dropdown in the panel becomes the shared custom control: what is in the DOM now, plus
// whatever the views create later (observed), so no view ever opts in or out.
initSelects();
// Only matters while the preference is "auto"; a fixed choice is not the OS's business.
if (window.matchMedia as unknown) {
  var mq = matchMedia("(prefers-color-scheme: dark)");
  var onOsChange = function () { if (themePref() === "auto") { applyTheme(); paintThemeBtn(); } };
  if (mq.addEventListener) mq.addEventListener("change", onOsChange);
  else if (mq.addListener) mq.addListener(onOsChange);   // Safari < 14
}


/* Navigation and deep links are owned by the page registry. */

/* Polling is cheap: /api/mcps is status-only, and /api/memory reads process.memoryUsage() in-process.
   The child-subtree walk asked for below is the one costly part, and the server bounds it — cached for
   20s, deduped while in flight, and skipped outright when no proc MCP is running — so a 6s poll costs
   at most one transient powershell per 20s, and nothing at all when there are no children. Polling
   still pauses when the tab is hidden: nobody is looking. */
function poll(): void {
  if (document.visibilityState !== "visible") return;
  loadMemory(true);
  loadInfo();
  void pollPage();
}

setInterval(poll, 6000);
document.addEventListener("visibilitychange", function () { if (document.visibilityState === "visible") poll(); });
// Closing the page with buffered (uncommitted) edits would lose them silently; the native
// dialog is the only hook available for beforeunload, so it is plain by necessity.
window.addEventListener("beforeunload", function (e) {
  if (pageHasPendingChanges()) {
    e.preventDefault(); // Chrome needs this; the returnValue fallback covers the rest
    e.returnValue = "";
  }
});

// The toolbar refresh button is gone: every view already reloads on the 6s poll, so the
// button duplicated it. The memory chip's click re-reads memory only (polling.js
// refreshMemoryNow — a reading is not a reload button); the ONE explicit view refresh left
// is the r key below, which keeps Data's manual reload alive.

$<FilterInput>("filter").oninput = function () { state.filter = this.value; patchSidebar(); };
initImmersive(); // the context bar's focus control: the page body can take the whole window

/* Keyboard: arrows move through the sidebar, / focuses search, Escape closes the sheet/menu,
   then the history popover, and leaves immersive mode last — the outermost layer goes last. */
document.addEventListener("keydown", function (e) {
  if (e.key === "Escape") {
    if (!$("sheet").hidden) { closeSheet(); return; }
    if (state.menuOpen) { closeMenu(); return; }
    var hd = state.detail;
    if (hd && hd.run && hd.run.histOpen) { histClose(); return; }
    if (immersiveOn()) { exitImmersive(); return; }
  }
  var typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement && document.activeElement.tagName);
  if (typing) return;
  if (e.key === "r") { refreshNow(); return; }
  if (!pageUsesSidebar()) return;
  if (e.key === "/") { e.preventDefault(); $("filter").focus(); return; }
  // Alt+arrows move the selected MCP through the list (the drag-free path to the same reorder).
  if (e.altKey && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
    e.preventDefault();
    nudgeSelected(e.key === "ArrowUp");
    return;
  }
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  // navRows, not visibleMcps: the arrows walk what is actually on screen, so a folded group is
  // stepped over rather than selected into.
  var rows = navRows();
  if (!rows.length) return;
  e.preventDefault();
  var i = rows.findIndex(function (m) { return m.name === state.selected; });
  var nextIndex = e.key === "ArrowDown" ? Math.min(rows.length - 1, i + 1) : Math.max(0, i - 1);
  if (i < 0) nextIndex = 0;
  openDetail(rows[nextIndex].name);
});

state.collapsed = loadCollapsed("mcps"); // before the first paint, so folded groups never flash open
state.tun.collapsed = { conns: loadCollapsed("conns"), rules: loadCollapsed("rules") }; // same, per tunnels page scope
state.jobs.collapsed = loadCollapsed("jobs"); // the jobs scope's own fold map (docs/20 G4)
showApp();
// Ask for the child walk on the very first paint too, so the chip never shows a gateway-only total
// that a poll silently corrects 6s later.
loadMemory(true);
