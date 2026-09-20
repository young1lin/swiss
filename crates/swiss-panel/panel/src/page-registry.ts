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

import type { ApiPluginPage, ApiPluginRow, ApiPluginsResponse } from "./types/api.js";
import type { MenuItemAction, PageDescriptor, PageGroup, PageMenuItemSpec, PageModule } from "./types/dom.js";
import { $, api, errText, iconNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import { createPageRegistry } from "./page-core.js";
import { glyphNode, openPluginPalette, pinnedGroups } from "./plugin-palette.js";
import { loadLastPages, rememberLastPage, targetPageFor } from "./last-page.js";
import { currentView, setCurrentView } from "./ui-state.js";

/* Older gateways use this single manifest; a plugin-aware host supplies the same descriptors. */
const legacy: PageDescriptor[] = [
  { id: "mcps", pluginId: "mcp", label: "MCPs", sidebar: true },
  { id: "traffic", pluginId: "mcp", label: "Traffic" },
  { id: "tokens", pluginId: "mcp", label: "Token" },
  { id: "tunnels", pluginId: "tunnels", label: "SSH Connections" },
  { id: "tunnel-forwards", pluginId: "tunnels", label: "Port Forwards" },
  { id: "data", pluginId: "data", label: "Data" },
  { id: "jobs", pluginId: "jobs", label: "Jobs" },
].map((p, i) => { return Object.assign({ order: i * 10, path: "#" + p.id, entry: "/admin/js/views/" + p.id + ".js" }, p); });
const management: PageDescriptor = { id: "plugins", pluginId: "host", label: "Plugins", order: 1000, path: "#plugins", entry: "/admin/js/views/plugins.js" };
/* These pages are host-owned like management: every plugin may depend on the vault, while System
 * controls the one running process rather than any individual plugin. */
const vaultPage: PageDescriptor = { id: "secrets", pluginId: "host", label: "Secrets", order: 1001, path: "#secrets", entry: "/admin/js/views/secrets.js" };
const systemPage: PageDescriptor = { id: "system", pluginId: "host", label: "System", order: 1002, path: "#system", entry: "/admin/js/views/system.js" };
/* Group labels used when the host serves no plugin inventory (an older gateway answers 404 on
   /api/plugins), plus the one group that has no inventory row at all: the management page is
   synthesized here, not contributed by a plugin. */
const GROUP_LABELS = { mcp: "MCP", tunnels: "Tunnels", data: "Data", jobs: "Jobs", host: "Settings" };
const registry = createPageRegistry();
registry.replace(legacy);
let inventory: ApiPluginsResponse | null = null;
let active: { id: string; module: PageModule; controller: AbortController } | null = null;
let sequence = 0;
let boot: Promise<void> | null = null;
let polling = false;
/* The one tab-strip width watcher (docs/39 S3): re-created never, re-pointed every repaint. */
let tabsObserver: ResizeObserver | null = null;

function pluginInventory(): ApiPluginsResponse | null { return inventory; }
function pageHasPendingChanges() { return !!(active && active.module.hasPendingChanges && active.module.hasPendingChanges()); }
function pageUsesSidebar() { return layoutOf(registry.get(currentView())) === "resource"; }
function currentPageCount() { return active && active.module.countText ? active.module.countText() : ""; }
function pluginFor(page: PageDescriptor): ApiPluginRow | null | undefined { return inventory && (inventory.plugins || []).find((p) => { return p.id === page.pluginId; }); }
function unavailable(page: PageDescriptor): ApiPluginRow | null {
  const plugin = pluginFor(page);
  return plugin && (plugin.enabled === false || ["disabled", "failed", "waitingDependency", "not-built"].includes(plugin.state)) ? plugin : null;
}

/* --- layouts (docs/13 D5, as revised) -------------------------------------------------------------
   How a page's BODY is framed, from the descriptor when the gateway states it and derived
   from sidebar when it does not (an older gateway): resource = master-detail that owns the
   panel's sidebar; page = an ordinary content body; workspace = a full-bleed, dense body
   (the terminal, the data explorer). Workspace changes body framing ONLY - the shell
   always draws its own chrome (rail and context bar) around every layout, so nothing here
   decides who owns the chrome anymore. Workspace is never guessed - no plugin accidentally
   loses the resource sidebar to a fallback. */
function layoutOf(page: PageDescriptor | null | undefined): string {
  if (page && (page.layout === "resource" || page.layout === "page" || page.layout === "workspace")) return page.layout;
  return page && page.sidebar ? "resource" : "page";
}

function currentGroups(): PageGroup[] { return registry.groups(inventory && inventory.plugins, GROUP_LABELS); }
function currentGroup(): PageGroup | undefined {
  return currentGroups().find((g) => { return g.pages.some((p) => { return p.id === currentView(); }); });
}

/* --- the plugin rail: global navigation (level one) ----------------------------------------------
   One seat per PINNED plugin group, plus the "..." seat that opens the palette (the full,
   searchable list - the rail is the shortlist, not the ceiling). A seat carries data-group
   and data-view (the group's lowest-order page) so the deep selector in jobs.js keeps
   matching, and clicks delegate on [data-group] - the page actually opened is computed per
   click (seatTarget, docs/39 S4), never written into the markup. */
function railSeat(g: PageGroup): HTMLElement {
  const active = g.pages.some((p) => { return p.id === currentView(); });
  const offPlugin = unavailable(g.pages[0]);
  const allOff = g.pages.every((p) => { return !!unavailable(p); });
  const title = g.label + (allOff && offPlugin ? " — " + (offPlugin.lastError || "Plugin disabled") : "");
  /* Icon-only (docs/39 S1): the seat wears just the glyph; the plugin's name lives in the
   * seat's title (the tooltip) and, once landed, in the context bar's title. */
  return h("button", { class: "rail-btn", data: { group: g.id, view: g.pages[0].id },
      // The two aria flags render only when true, exactly as the string builder spelled them.
      aria: Object.assign({}, active ? { current: "true" } : {}, allOff ? { disabled: "true" } : {}), title },
    glyphNode(g));
}

function moreSeat(): HTMLElement {
  return h("button", { class: "rail-btn rail-more", id: "railMore", type: "button", title: "All plugins", aria: { label: "All plugins" } },
    iconNode("ellipsis"));
}

function paintPluginRail(): void {
  const nav = $("railNav");
  fill(nav, ...pinnedGroups(currentGroups()).map(railSeat), moreSeat());
  nav.onclick = (event) => {
    const target = event.target as HTMLElement;
    const more = target.closest<HTMLElement>(".rail-more");
    if (more) { openPluginPalette(decoratedGroups(), (group) => { void navigatePage(seatTarget(group)); }, paintPluginRail); return; }
    const button = target.closest<HTMLElement>("[data-group]");
    const group = button && currentGroups().find((g) => { return g.id === button.dataset.group; });
    if (group) void navigatePage(seatTarget(group));
  };
}

/** The page a plugin's seat or palette row opens (docs/39 S4): the last page visited when
 *  it is still a member of the group and usable, else the group's first page. Both entry
 *  points go through this one wrapper, so the policy exists exactly once. */
function seatTarget(group: { id: string; pages: { id: string }[] }): string {
  return targetPageFor(group, loadLastPages(), (id) => {
    const page = registry.get(id);
    return !!page && !unavailable(page);
  });
}

/* Groups carrying their availability, for the palette: it is navigation for EVERY plugin,
 * including the ones the host currently refuses to serve - they stay reachable, marked. */
function decoratedGroups() {
  return currentGroups().map((g) => {
    const bad = unavailable(g.pages[0]);
    return {
      id: g.id, label: g.label, pages: g.pages,
      off: g.pages.length > 0 && g.pages.every((p) => { return !!unavailable(p); }),
      offDetail: bad ? (bad.lastError || "Plugin disabled") : "",
    };
  });
}

/* --- the plugin context bar: page navigation (level two) ------------------------------------------
   ALWAYS drawn in normal mode (docs/13 D5, as revised; docs/39 S2): the same bar height
   and the same body origin for every plugin - multi-page, single-page and workspace alike.
   The left half names the plugin ONCE - the same glyph its rail seat wears, beside its
   label - and a multi-page plugin lays its pages out as underline tabs, every page visible
   at once instead of parked behind a menu: three levels, three shapes (a rail seat, an
   underlined tab, a pill segment inside the page), so level one and level two never share
   a control shape. A single-page plugin draws only the title - no fake one-entry tabs. */
/** The ⋯ menu's items for one group: its pages in order, the current one picked,
 *  unavailable ones marked. Factored out of the click handler so the contract (order, the
 *  pick column, the · off marker) is testable without a menu. */
function pageMenuItems(current: PageGroup): PageMenuItemSpec[] {
  return current.pages.map((p) => {
    const po = unavailable(p);
    return {
      label: p.label + (po ? " · off" : ""),
      title: po ? (po.lastError || "Plugin disabled") : undefined,
      pick: true,
      on: p.id === currentView(),
    };
  });
}

/** One underline tab (docs/39 S2/S8): a real link through the hash, so keyboard focus,
 *  middle-click and copy-link come free and the existing hashchange path does the
 *  navigating. Unavailable pages stay listed and marked - the same contract the old
 *  switcher menu had - and still navigate (the landing paints the unavailable state). */
function tabNode(p: PageDescriptor, active: boolean): HTMLAnchorElement {
  const po = unavailable(p);
  const aria: Record<string, string> = {};
  if (active) aria.current = "page";
  if (po) aria.disabled = "true";
  return h("a", {
    class: "ctx-tab", href: p.path || "#" + p.id,
    title: po ? (po.lastError || "Plugin disabled") : "",
    aria: aria, data: { page: p.id },
  }, p.label + (po ? " · off" : ""));
}

/** The overflow seat (docs/39 S3): the ⋯ tab that appears when the pages do not fit. It
 *  reuses the old switcher's menu exactly - pageMenuItems lists every page, the current
 *  one picked - so the safety net and the tabs can never disagree about what exists. */
function moreTab(current: PageGroup): HTMLButtonElement {
  const b = h("button", { class: "ctx-tab ctx-more", type: "button", title: "All pages", aria: { haspopup: "menu", expanded: "false" } }, iconNode("ellipsis"));
  b.onclick = (ev) => {
    ev.stopPropagation();
    b.setAttribute("aria-expanded", "true");
    const items = pageMenuItems(current).map((it, i) => {
      const p = current.pages[i];
      it.fn = () => { b.setAttribute("aria-expanded", "false"); void navigatePage(p.id); };
      return it as MenuItemAction;
    });
    /* menu.js's module graph wires DOM at import time (add-sheet binds its buttons at the
     * top level), so it loads HERE, at interaction time - the shell's own module graph stays
     * DOM-free at eval, which the pure-helper suites (plugins.js) import it under. */
    void import("./menu.js").then((menu) => { menu.popupMenu(b.getBoundingClientRect(), items); });
    /* The menu also closes without an item click (document click, Escape); a one-shot
     * listener puts the flag back whenever that lands. */
    setTimeout(() => {
      document.addEventListener("click", () => { b.setAttribute("aria-expanded", "false"); }, { once: true });
    });
  };
  return b;
}

/** Which tabs stay visible in `avail` px (docs/39 S3). All fit -> all visible. Otherwise
 *  reserve `moreWidth` for the ⋯ tab, keep tabs in order while they fit, and guarantee
 *  the active one: when it fell into the overflow it takes the last visible slot. Returns
 *  ids; the caller toggles `hidden`. Pure - the suite pins each rule with numbers. */
function fitTabs(tabs: { id: string; width: number }[], activeId: string, avail: number, moreWidth: number): { visible: string[]; overflow: string[] } {
  const total = tabs.reduce((sum, t) => { return sum + t.width; }, 0);
  if (total <= avail) return { visible: tabs.map((t) => { return t.id; }), overflow: [] };
  const budget = avail - moreWidth;
  let used = 0;
  let cut = tabs.length;
  for (let i = 0; i < tabs.length; i++) {
    if (used + tabs[i].width > budget) { cut = i; break; }
    used += tabs[i].width;
  }
  const visible = tabs.slice(0, cut).map((t) => { return t.id; });
  const overflow = tabs.slice(cut).map((t) => { return t.id; });
  if (activeId && overflow.indexOf(activeId) !== -1) {
    overflow.splice(overflow.indexOf(activeId), 1);
    const displaced = visible.pop();
    if (displaced != null) overflow.unshift(displaced);
    visible.push(activeId);
  }
  return { visible: visible, overflow: overflow };
}

/** Measure the painted tabs and hide what does not fit (docs/39 S3): every width comes
 *  from the laid-out nodes, the decision from fitTabs. The ⋯ seat is measured by un-hiding
 *  it for the read - a display:none box has no width - then hidden again unless needed.
 *  Environments without layout (the vitest node DOM: every offsetWidth is 0) degrade to
 *  "everything fits", which is exactly what the suite asserts. */
function layoutTabs(): void {
  const tabs = $("pageTabs");
  /* Measuring needs the box model (element children, a scoped querySelector, box widths).
   * A DOM without it - the parse-and-eval boot suite's minimal stub - keeps every tab
   * visible: nothing can be measured, so nothing is hidden. */
  if (!tabs.children || typeof tabs.querySelector !== "function") return;
  const links = (Array.prototype.filter.call(tabs.children, (el: HTMLElement) => { return el.classList.contains("ctx-tab") && !el.classList.contains("ctx-more"); }) as HTMLElement[]);
  const more = tabs.querySelector<HTMLElement>(".ctx-more");
  if (!links.length || !more) return;
  more.hidden = false;
  const measured = links
    .map((el) => { return { id: el.dataset.page || "", width: el.offsetWidth || 0 }; })
    .filter((t) => { return t.id !== ""; });
  const fit = fitTabs(measured, currentView(), tabs.clientWidth || 0, more.offsetWidth || 0);
  links.forEach((el) => {
    const id = el.dataset.page || "";
    if (id !== "") el.hidden = fit.visible.indexOf(id) === -1;
  });
  more.hidden = fit.overflow.length === 0;
}

function paintPluginContext(): void {
  const bar = $("ctxBar");
  const title = $("pageTitle");
  const tabs = $("pageTabs");
  const current = currentGroup();
  const page = registry.get(currentView());
  /* No page to name (an empty registry during boot) is the one case with nothing to draw;
   * every real navigation lands in a group and keeps the bar. Focus mode keeps its minimal
   * bar in normal flow because #expandBtn lives inside it; hiding the parent would strand
   * the exit with Esc as the only way out. An obvious exit beats an empty bar flashing for
   * one boot frame. */
  const show = !!(current && page);
  const immersive = document.body && document.body.classList && document.body.classList.contains("immersive");
  bar.hidden = !show && !immersive;
  if (!show) {
    title.hidden = true;
    tabs.hidden = true;
    if (tabsObserver) tabsObserver.disconnect();
    return;
  }
  const off = unavailable(page!);
  const current_ = current!;
  /* The plugin's name lives here once (docs/39 S2): its rail glyph beside its label, so an
   * icon-only rail still names the destination and the title can never read as a tab. */
  title.hidden = false;
  title.title = off ? (off.lastError || "Plugin disabled") : "";
  fill(title, glyphNode(current_), h("span", { class: "ctx-name" }, current_.label));
  if (current_.pages.length >= 2) {
    tabs.hidden = false;
    fill(tabs, ...current_.pages.map((p) => { return tabNode(p, p.id === currentView()); }), moreTab(current_));
    layoutTabs();
    /* Re-fit when the bar's width changes (the window, the sidebar): one observer, moved to
     * the freshly painted node on every repaint - the old node is gone with its listeners,
     * and a disconnected box must never keep firing. Guarded: the node test DOM has none. */
    if (typeof ResizeObserver !== "undefined") {
      if (!tabsObserver) tabsObserver = new ResizeObserver(() => { layoutTabs(); });
      tabsObserver.disconnect();
      tabsObserver.observe(tabs);
    }
    /* The paint-time measure can run against a pre-frame layout - the bar's count chip
     * fills only after the view paints - and an observer re-attached at an unchanged
     * box size reports nothing when that content-driven shrink lands inside the same
     * frame (seen live at 480px: the strip kept its wide fit until the window moved).
     * One deferred re-fit after the frame settles closes both; bounded, never chained. */
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => { requestAnimationFrame(() => { layoutTabs(); }); });
    }
  } else {
    tabs.hidden = true;
    if (tabsObserver) tabsObserver.disconnect();
  }
}

function paintNavigation(): void {
  paintPluginRail();
  paintPluginContext();
}

async function reloadPluginInventory(): Promise<ApiPluginsResponse | null> {
  const response = await api("/api/plugins");
  if (response.status === 404) {
    inventory = null;
    registry.replace(legacy);
  } else {
    if (!response.ok) throw new Error("Cannot load plugin inventory: HTTP " + response.status);
    const next: ApiPluginsResponse & { plugins?: (ApiPluginRow & { pages?: ApiPluginPage[] })[] } = await response.json();
    const pages = next.pages || (next.plugins || []).flatMap((p) => { return p.pages || []; });
    registry.replace(pages.filter((p) => { return !["plugins", "secrets", "system"].includes(p.id); })
      .concat([management, vaultPage, systemPage] as ApiPluginPage[]));
    inventory = next;
  }
  paintNavigation();
  return inventory;
}

function initPages(): Promise<void> {
  if (boot) return boot;
  boot = (async () => {
    try { await reloadPluginInventory(); }
    catch (error) { toast(errText(error), true); paintNavigation(); }
    let wanted = (location.hash || "#mcps").replace(/^#\/?/, "");
    if (!registry.get(wanted)) wanted = registry.list()[0] && registry.list()[0].id;
    if (wanted) await navigatePage(wanted, true);
    window.addEventListener("hashchange", () => {
      const id = location.hash.replace(/^#\/?/, "");
      if (registry.get(id)) void navigatePage(id);
    });
  })();
  return boot;
}

async function navigatePage(id: string, force?: boolean): Promise<void> {
  const page = registry.get(id);
  if (!page || (!force && active && active.id === id)) return;
  if (active && active.module.canLeave && !active.module.canLeave()) {
    history.replaceState(null, "", "#" + active.id);
    return;
  }
  const ticket = ++sequence;
  let module: PageModule;
  const off = unavailable(page);
  try { module = off ? {} : await registry.load(id); }
  catch (error) { toast(errText(error), true); return; }
  if (ticket !== sequence) return;
  if (active) {
    active.controller.abort();
    if (active.module.unmount) await active.module.unmount();
  }
  const controller = new AbortController();
  setCurrentView(id);
  /* docs/39 S4: the plugin's seat reopens here. Keyed by the GROUP id (currentGroup()'s),
   * not page.pluginId - legacy descriptors may omit the field and groups() homogenized it
   * already. Only a navigation that LANDED records: the canLeave early-return above never
   * reaches this line, so a blocked leave keeps the previous memory intact. */
  const landed = currentGroup();
  if (landed) rememberLastPage(landed.id, id);
  active = { id: id, module: module, controller: controller };
  /* The resource sidebar belongs to the resource layout alone; every other layout gets the
   * full body width (a "page" wraps its content in the pane's padding, a "workspace" is
   * full-bleed - a framing difference only, the shell's chrome is drawn either way). */
  document.querySelector<HTMLElement>(".sidebar")!.hidden = layoutOf(page) !== "resource";
  paintNavigation();
  history.replaceState(null, "", page.path || "#" + id);
  fill($("pane"), off
    ? h("div", { class: "empty" }, h("div", null,
        h("h2", null, page.label, " unavailable"),
        h("p", { class: "hint" }, off.lastError || "This plugin is disabled. Manage it in Plugins.")))
    : h("div", { class: "empty" }, "Loading " + page.label + "…"));
  try { if (module.mount) await module.mount({ signal: controller.signal }); }
  catch (error) { if (ticket === sequence && error instanceof Error && error.name !== "AbortError") toast(errText(error), true); }
  if (ticket === sequence) $("countChip").textContent = currentPageCount();
}

async function pollPage(): Promise<void> {
  if (polling || !active || !active.module.poll) return;
  polling = true;
  try { await active.module.poll(); }
  catch (error) { toast(errText(error), true); }
  finally { polling = false; }
}
async function refreshPage(): Promise<void> {
  if (!active) return;
  try { if (active.module.refresh) await active.module.refresh(); else if (active.module.poll) await active.module.poll(); }
  catch (error) { toast(errText(error), true); }
}

export { currentPageCount, fitTabs, initPages, layoutOf, navigatePage, pageHasPendingChanges, pageMenuItems, pageUsesSidebar, pluginInventory, pollPage, refreshPage, reloadPluginInventory };
