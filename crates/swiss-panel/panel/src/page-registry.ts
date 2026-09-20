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
import { currentView, setCurrentView } from "./ui-state.js";
import { tk, tr, wireLabel } from "./i18n.js";

/* Older gateways use this single manifest; a plugin-aware host supplies the same descriptors. */
const legacy: PageDescriptor[] = [
  { id: "mcps", pluginId: "mcp", label: tk("pageRegistry.mcps"), sidebar: true },
  { id: "traffic", pluginId: "mcp", label: tk("pageRegistry.traffic") },
  { id: "tokens", pluginId: "mcp", label: tk("pageRegistry.token") },
  { id: "tunnels", pluginId: "tunnels", label: tk("pageRegistry.sshConnections") },
  { id: "tunnel-forwards", pluginId: "tunnels", label: tk("pageRegistry.portForwards") },
  { id: "data", pluginId: "data", label: tk("pageRegistry.data") },
  { id: "jobs", pluginId: "jobs", label: tk("pageRegistry.jobs") },
].map((p, i) => { return Object.assign({ order: i * 10, path: "#" + p.id, entry: "/admin/js/views/" + p.id + ".js" }, p); });
const management: PageDescriptor = { id: "plugins", pluginId: "host", label: tk("pageRegistry.plugins"), order: 1000, path: "#plugins", entry: "/admin/js/views/plugins.js" };
/* These pages are host-owned like management: every plugin may depend on the vault, while System
 * controls the one running process rather than any individual plugin. */
const vaultPage: PageDescriptor = { id: "secrets", pluginId: "host", label: tk("pageRegistry.secrets"), order: 1001, path: "#secrets", entry: "/admin/js/views/secrets.js" };
const systemPage: PageDescriptor = { id: "system", pluginId: "host", label: tk("pageRegistry.system"), order: 1002, path: "#system", entry: "/admin/js/views/system.js" };
/* Group labels used when the host serves no plugin inventory (an older gateway answers 404 on
   /api/plugins), plus the one group that has no inventory row at all: the management page is
   synthesized here, not contributed by a plugin. */
const GROUP_LABELS = { mcp: tk("pageRegistry.mcp"), tunnels: tk("pageRegistry.tunnels"), data: tk("pageRegistry.data"), jobs: tk("pageRegistry.jobs"), host: tk("pageRegistry.settings") };
const registry = createPageRegistry();
registry.replace(legacy);
let inventory: ApiPluginsResponse | null = null;
let active: { id: string; module: PageModule; controller: AbortController } | null = null;
let sequence = 0;
let boot: Promise<void> | null = null;
let polling = false;

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
   matching, and clicks delegate on [data-view]. */
function railSeat(g: PageGroup): HTMLElement {
  const active = g.pages.some((p) => { return p.id === currentView(); });
  const offPlugin = unavailable(g.pages[0]);
  const allOff = g.pages.every((p) => { return !!unavailable(p); });
  const label = tr(wireLabel(g.label));
  const title = allOff && offPlugin
    ? tr("pageRegistry.labelError", { label, error: offPlugin.lastError || tr("pageRegistry.pluginDisabled") })
    : label;
  return h("button", { class: "rail-btn", data: { group: g.id, view: g.pages[0].id },
      // The two aria flags render only when true, exactly as the string builder spelled them.
      aria: Object.assign({}, active ? { current: "true" } : {}, allOff ? { disabled: "true" } : {}), title },
    glyphNode(g),
    h("span", { class: "rail-btn-label" }, label));
}

function moreSeat(): HTMLElement {
  return h("button", { class: "rail-btn rail-more", id: "railMore", type: "button", title: tr("pageRegistry.allPlugins"), aria: { label: tr("pageRegistry.allPlugins") } },
    iconNode("ellipsis"),
    h("span", { class: "rail-btn-label" }, tr("pageRegistry.more")));
}

function paintPluginRail(): void {
  const nav = $("railNav");
  fill(nav, ...pinnedGroups(currentGroups()).map(railSeat), moreSeat());
  nav.onclick = (event) => {
    const target = event.target as HTMLElement;
    const more = target.closest<HTMLElement>(".rail-more");
    if (more) { openPluginPalette(decoratedGroups(), navigatePage, paintPluginRail); return; }
    const button = target.closest<HTMLElement>("[data-view]");
    if (button) void navigatePage(button.dataset.view!);
  };
}

/* Groups carrying their availability, for the palette: it is navigation for EVERY plugin,
 * including the ones the host currently refuses to serve - they stay reachable, marked. */
function decoratedGroups() {
  return currentGroups().map((g) => {
    const bad = unavailable(g.pages[0]);
    return {
      id: g.id, label: g.label, pages: g.pages,
      off: g.pages.length > 0 && g.pages.every((p) => { return !!unavailable(p); }),
      offDetail: bad ? (bad.lastError || tr("pageRegistry.pluginDisabled")) : "",
    };
  });
}

/* --- the plugin context bar: page navigation (level two) ------------------------------------------
   ALWAYS drawn in normal mode (docs/13 D5, as revised): the same bar height and the same
   body origin for every plugin - multi-page, single-page and workspace alike. A multi-page
   plugin gets the compact "MCP / Servers" switcher with a menu, never a full-width second
   tab row - level one and level two never share a control shape. A single-page plugin gets
   a static location label instead (#pageLoc): no fake dropdown, no "Data / Data" - the
   location reads as text because it is text. */
/** The switcher menu's items for one group: its pages in order, the current one picked,
 *  unavailable ones marked. Factored out of the click handler so the contract (order, the
 *  pick column, the · off marker) is testable without a menu. */
function pageMenuItems(current: PageGroup): PageMenuItemSpec[] {
  return current.pages.map((p) => {
    const po = unavailable(p);
    return {
      label: tr(wireLabel(p.label)) + (po ? tr("pageRegistry.off") : ""),
      title: po ? (po.lastError || tr("pageRegistry.pluginDisabled")) : undefined,
      pick: true,
      on: p.id === currentView(),
    };
  });
}

function paintPluginContext(): void {
  const bar = $("ctxBar");
  const btn = $("pageBtn");
  const loc = $("pageLoc");
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
    btn.hidden = true; btn.onclick = null;
    loc.hidden = true;
    return;
  }
  const off = unavailable(page!);
  const current_ = current!;
  if (current_.pages.length >= 2) {
    btn.hidden = false;
    loc.hidden = true;
    fill(btn,
      h("span", { class: "ctx-plugin" }, tr(wireLabel(current_.label))),
      h("span", { class: "ctx-sep" }, "/"),
      h("span", { class: "ctx-page" }, tr(wireLabel(page?.label || ""))),
      iconNode("chevron-right"));
    btn.title = off ? (off.lastError || tr("pageRegistry.pluginDisabled")) : tr("pageRegistry.switchLabelPage", { label: tr(wireLabel(current_.label)) });
    btn.onclick = (ev) => {
      ev.stopPropagation();
      btn.setAttribute("aria-expanded", "true");
      const items = pageMenuItems(current!).map((it, i) => {
        const p = current?.pages[i];
        it.fn = () => { btn.setAttribute("aria-expanded", "false"); void navigatePage(p.id); };
        return it as MenuItemAction;
      });
      /* menu.js's module graph wires DOM at import time (add-sheet binds its buttons at the
       * top level), so it loads HERE, at interaction time - the shell's own module graph stays
       * DOM-free at eval, which the pure-helper suites (plugins.js) import it under. */
      void import("./menu.js").then((menu) => { menu.popupMenu(btn.getBoundingClientRect(), items); });
      /* The menu also closes without an item click (document click, Escape); a one-shot
       * listener puts the flag back whenever that lands. */
      setTimeout(() => {
        document.addEventListener("click", () => { btn.setAttribute("aria-expanded", "false"); }, { once: true });
      });
    };
  } else {
    /* One page: the plugin name IS the location. Same slot, same weight as the page label
     * of a multi-page group, but plain text - a dropdown with one entry is a lie about
     * what the control does. */
    btn.hidden = true;
    btn.onclick = null;
    btn.setAttribute("aria-expanded", "false");
    loc.hidden = false;
    fill(loc, h("span", { class: "ctx-page" }, tr(wireLabel(current_.label))));
    loc.title = off ? (off.lastError || tr("pageRegistry.pluginDisabled")) : "";
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
    if (!response.ok) throw new Error(tr("pageRegistry.cannotLoadPluginInventory", { status: response.status }));
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
  active = { id: id, module: module, controller: controller };
  /* The resource sidebar belongs to the resource layout alone; every other layout gets the
   * full body width (a "page" wraps its content in the pane's padding, a "workspace" is
   * full-bleed - a framing difference only, the shell's chrome is drawn either way). */
  document.querySelector<HTMLElement>(".sidebar")!.hidden = layoutOf(page) !== "resource";
  paintNavigation();
  history.replaceState(null, "", page.path || "#" + id);
  fill($("pane"), off
    ? h("div", { class: "empty" }, h("div", null,
        h("h2", null, tr("pageRegistry.pageUnavailable", { page: tr(wireLabel(page.label)) })),
        h("p", { class: "hint" }, off.lastError || tr("pageRegistry.pluginDisabledManagePlugins"))))
    : h("div", { class: "empty" }, tr("pageRegistry.loadingPage", { page: tr(wireLabel(page.label)) })));
  try { if (module.mount) await module.mount({ signal: controller.signal }); }
  catch (error) { if (ticket === sequence && error instanceof Error && error.name !== "AbortError") toast(errText(error), true); }
  if (ticket === sequence) $("countChip").textContent = currentPageCount();
}

/** Whether the active page would allow a navigation right now (docs/38 §2.2): the 文/A
 *  flip asks this before it writes the language preference — a vetoed switch must not
 *  change anything the user can observe. */
function pageCanLeave(): boolean { return !(active && active.module.canLeave && !active.module.canLeave()); }

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

export { currentPageCount, initPages, layoutOf, navigatePage, pageCanLeave, pageHasPendingChanges, pageMenuItems, pageUsesSidebar, pluginInventory, pollPage, refreshPage, reloadPluginInventory };