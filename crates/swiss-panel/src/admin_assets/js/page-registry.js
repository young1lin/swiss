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

                                                                                      
                                                                                                              
import { $, api, errText, iconNode, toast } from "./util.js";
import { fill, h } from "./h.js";
import { createPageRegistry } from "./page-core.js";
import { glyphNode, openPluginPalette, pinnedGroups } from "./plugin-palette.js";
import { loadLastPages, rememberLastPage, targetPageFor } from "./last-page.js";
import { currentView, setCurrentView } from "./ui-state.js";
import { resetPaneScroll, trackPaneScroll } from "./pane-scroll.js";
import { tk, tr, wireLabel } from "./i18n.js";

/* Older gateways use this single manifest; a plugin-aware host supplies the same descriptors. */
const legacy                   = [
  { id: "mcps", pluginId: "mcp", label: tk("pageRegistry.mcps"), sidebar: true },
  { id: "traffic", pluginId: "mcp", label: tk("pageRegistry.traffic") },
  { id: "tokens", pluginId: "mcp", label: tk("pageRegistry.token") },
  { id: "tunnels", pluginId: "tunnels", label: tk("pageRegistry.sshConnections") },
  { id: "tunnel-forwards", pluginId: "tunnels", label: tk("pageRegistry.portForwards") },
  { id: "data", pluginId: "data", label: tk("pageRegistry.data") },
  { id: "jobs", pluginId: "jobs", label: tk("pageRegistry.jobs") },
].map((p, i) => { return Object.assign({ order: i * 10, path: "#" + p.id, entry: "/admin/js/views/" + p.id + ".js" }, p); });
const management                 = { id: "plugins", pluginId: "host", label: tk("pageRegistry.plugins"), order: 1000, path: "#plugins", entry: "/admin/js/views/plugins.js" };
/* These pages are host-owned like management: every plugin may depend on the vault, while System
 * controls the one running process rather than any individual plugin. */
const vaultPage                 = { id: "secrets", pluginId: "host", label: tk("pageRegistry.secrets"), order: 1001, path: "#secrets", entry: "/admin/js/views/secrets.js" };
const systemPage                 = { id: "system", pluginId: "host", label: tk("pageRegistry.system"), order: 1002, path: "#system", entry: "/admin/js/views/system.js" };
/* Group labels used when the host serves no plugin inventory (an older gateway answers 404 on
   /api/plugins), plus the one group that has no inventory row at all: the management page is
   synthesized here, not contributed by a plugin. */
const GROUP_LABELS = { mcp: tk("pageRegistry.mcp"), tunnels: tk("pageRegistry.tunnels"), data: tk("pageRegistry.data"), jobs: tk("pageRegistry.jobs"), host: tk("pageRegistry.settings") };
const registry = createPageRegistry();
registry.replace(legacy);
let inventory                            = null;
let active                                                                         = null;
let sequence = 0;
let boot                       = null;
let polling = false;
/* The one tab-strip width watcher (docs/39 S3): re-created never, re-pointed every repaint. */
let tabsObserver                        = null;

function pluginInventory()                            { return inventory; }
function pageHasPendingChanges() { return !!(active && active.module.hasPendingChanges && active.module.hasPendingChanges()); }
function pageUsesSidebar() { return layoutOf(registry.get(currentView())) === "resource"; }
function currentPageCount() { return active && active.module.countText ? active.module.countText() : ""; }
function pluginFor(page                )                                  { return inventory && (inventory.plugins || []).find((p) => { return p.id === page.pluginId; }); }
function unavailable(page                )                      {
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
function layoutOf(page                                   )         {
  if (page && (page.layout === "resource" || page.layout === "page" || page.layout === "workspace")) return page.layout;
  return page && page.sidebar ? "resource" : "page";
}

function currentGroups()              { return registry.groups(inventory && inventory.plugins, GROUP_LABELS); }
function currentGroup()                        {
  return currentGroups().find((g) => { return g.pages.some((p) => { return p.id === currentView(); }); });
}

/* --- the plugin rail: global navigation (level one) ----------------------------------------------
   One seat per PINNED plugin group, plus the "..." seat that opens the palette (the full,
   searchable list - the rail is the shortlist, not the ceiling). A seat carries data-group
   and data-view (the group's lowest-order page) so the deep selector in jobs.js keeps
   matching, and clicks delegate on [data-group] - the page actually opened is computed per
   click (seatTarget, docs/39 S4), never written into the markup. */
function railSeat(g           )              {
  const active = g.pages.some((p) => { return p.id === currentView(); });
  const offPlugin = unavailable(g.pages[0]);
  const allOff = g.pages.every((p) => { return !!unavailable(p); });
  const title = allOff && offPlugin
    ? tr("pageRegistry.labelError", { label: tr(wireLabel(g.label)), error: offPlugin.lastError || tr("pageRegistry.pluginDisabled") })
    : tr(wireLabel(g.label));
  /* docs/39 S1 shipped the rail icon-only; reversed by owner decision (2026-10-30) for
   * clarity - the seat wears its glyph AND the plugin's translated name, sized for the
   * whole rail by fitRailLabels after each paint. The tooltip still carries the richer
   * error text when the plugin is down; the label stays the plain name. The More seat
   * keeps its glyph alone - it is an affordance, not a domain. */
  return h("button", { class: "rail-btn", data: { group: g.id, view: g.pages[0].id },
      // The two aria flags render only when true, exactly as the string builder spelled them.
      aria: Object.assign({}, active ? { current: "true" } : {}, allOff ? { disabled: "true" } : {}), title },
    glyphNode(g),
    h("span", { class: "rail-label" }, tr(wireLabel(g.label))));
}

function moreSeat()              {
  return h("button", { class: "rail-btn rail-more", id: "railMore", type: "button", title: tr("pageRegistry.allPlugins"), aria: { label: tr("pageRegistry.allPlugins") } },
    iconNode("ellipsis"));
}

/** One caption size for the whole rail, fitted to the longest name. The rail carries its
 * seats' names again (docs/39 S1 reversed by the owner) and a gateway-served plugin name
 * is unknowable at build time, so the size cannot be a constant - but it is ONE size: a
 * column of captions in mixed sizes reads as a mistake, so the rail steps down as one.
 * Captions start at RAIL_CAPTION_CEIL and the rail takes the largest half-pixel step at
 * which the longest name fits its seat's content box - the seat's padding is the air, and
 * the label's max-width (base.css) clips to that same box when nothing fits - floored at
 * RAIL_CAPTION_FLOOR - the size docs/39 recorded as the legibility floor. A name that does
 * not fit even there is clipped by the ellipsis rule and keeps its full text in the seat's
 * title. Measured once at the ceiling and written once: two reflows, never a loop. A rail
 * with no width (focus mode hides it) is left at the ceiling; the resize event that
 * leaving focus mode dispatches refits it, as does a real window resize. jsdom has no
 * layout (every width is 0), so the acceptance suite sees the ceiling. */
const RAIL_CAPTION_CEIL = 10;
const RAIL_CAPTION_FLOOR = 9;

/** The arithmetic of the fit, pure: each seat's `room` (the caption box, px) and `need`
 * (the caption's width at the ceiling, px). One size for all of them - the largest
 * half-pixel step at or below the ceiling at which every caption fits, floored. `null`
 * when nothing can be measured (a seat with no room is a hidden rail, not a tight one). */
function railCaptionSize(seats                                  )                {
  if (seats.length === 0 || seats.some((s) => s.room <= 0)) return null;
  const ratio = seats.reduce((r, s) => (s.need > s.room ? Math.min(r, s.room / s.need) : r), 1);
  return Math.max(RAIL_CAPTION_FLOOR, Math.floor(RAIL_CAPTION_CEIL * ratio * 2) / 2);
}

function fitRailLabels()       {
  const labels = Array.from(document.querySelectorAll             (".rail-btn .rail-label"));
  if (labels.length === 0) return;
  labels.forEach((el) => { el.style.fontSize = RAIL_CAPTION_CEIL + "px"; });
  // The seat's content box: its padding is the air (base.css .rail-btn), and the label's
  // max-width is 100% of this same box, so CSS and JS agree on what "fits" means.
  const size = railCaptionSize(labels.map((el) => {
    const seat = el.parentElement;
    if (!seat) return { room: 0, need: el.scrollWidth };
    const cs = getComputedStyle(seat);
    return { room: seat.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight), need: el.scrollWidth };
  }));
  if (size === null) return;
  labels.forEach((el) => { el.style.fontSize = size + "px"; });
}

let railRefitPending = false;
function scheduleRailRefit()       {
  if (railRefitPending) return;
  railRefitPending = true;
  requestAnimationFrame(() => { railRefitPending = false; fitRailLabels(); });
}

function paintPluginRail()       {
  const nav = $("railNav");
  fill(nav, ...pinnedGroups(currentGroups()).map(railSeat), moreSeat());
  fitRailLabels();
  nav.onclick = (event) => {
    const target = event.target               ;
    const more = target.closest             (".rail-more");
    if (more) { openPluginPalette(decoratedGroups(), (group) => { void navigatePage(seatTarget(group)); }, paintPluginRail); return; }
    const button = target.closest             ("[data-group]");
    const group = button && currentGroups().find((g) => { return g.id === button.dataset.group; });
    if (group) void navigatePage(seatTarget(group));
  };
}

/** The page a plugin's seat or palette row opens (docs/39 S4): the last page visited when
 *  it is still a member of the group and usable, else the group's first page. Both entry
 *  points go through this one wrapper, so the policy exists exactly once. */
function seatTarget(group                                         )         {
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
      offDetail: bad ? (bad.lastError || tr("pageRegistry.pluginDisabled")) : "",
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
function pageMenuItems(current           )                     {
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

/** One underline tab (docs/39 S2/S8): a real link through the hash, so keyboard focus,
 *  middle-click and copy-link come free and the existing hashchange path does the
 *  navigating. Unavailable pages stay listed and marked - the same contract the old
 *  switcher menu had - and still navigate (the landing paints the unavailable state). */
function tabNode(p                , active         )                    {
  const po = unavailable(p);
  const aria                         = {};
  if (active) aria.current = "page";
  if (po) aria.disabled = "true";
  return h("a", {
    class: "ctx-tab", href: p.path || "#" + p.id,
    title: po ? (po.lastError || tr("pageRegistry.pluginDisabled")) : "",
    aria: aria, data: { page: p.id },
  }, tr(wireLabel(p.label)) + (po ? tr("pageRegistry.off") : ""));
}

/** The overflow seat (docs/39 S3): the ⋯ tab that appears when the pages do not fit. It
 *  reuses the old switcher's menu exactly - pageMenuItems lists every page, the current
 *  one picked - so the safety net and the tabs can never disagree about what exists. */
function moreTab(current           )                    {
  const b = h("button", { class: "ctx-tab ctx-more", type: "button", title: tr("pageRegistry.allPages"), aria: { haspopup: "menu", expanded: "false" } }, iconNode("ellipsis"));
  b.onclick = (ev) => {
    ev.stopPropagation();
    b.setAttribute("aria-expanded", "true");
    const items = pageMenuItems(current).map((it, i) => {
      const p = current.pages[i];
      it.fn = () => { b.setAttribute("aria-expanded", "false"); void navigatePage(p.id); };
      return it                  ;
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
function fitTabs(tabs                                 , activeId        , avail        , moreWidth        )                                            {
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
 *  from the laid-out nodes, the decision from fitTabs. EVERYTHING is un-hidden for the
 *  read - a display:none box has no width, and a re-entry pass that measured only the
 *  visible tabs would always conclude "everything fits" and re-expand the strip (found
 *  live at 480px; the admin-navigation suite pins the refit). Environments without layout
 *  (the vitest node DOM: every offsetWidth is 0) degrade to "everything fits", which is
 *  exactly what the suite asserts. Exported for that suite's refit regression. */
export function layoutTabs()       {
  const tabs = $("pageTabs");
  /* Measuring needs the box model (element children, a scoped querySelector, box widths).
   * A DOM without it - the parse-and-eval boot suite's minimal stub - keeps every tab
   * visible: nothing can be measured, so nothing is hidden. */
  if (!tabs.children || typeof tabs.querySelector !== "function") return;
  const links = (Array.prototype.filter.call(tabs.children, (el             ) => { return el.classList.contains("ctx-tab") && !el.classList.contains("ctx-more"); })                 );
  const more = tabs.querySelector             (".ctx-more");
  if (!links.length || !more) return;
  /* Un-hide EVERYTHING before measuring, not just the ... seat: [hidden] is display:none
   * (base.css), so a tab hidden by a previous pass measures offsetWidth 0. Measuring a
   * stripped strip once made every later pass (observer, refit) see a total of visible tabs
   * only - always "everything fits" - and re-expand the strip over its own clip, stranding
   * the active tab out of view until a real window resize. Un-hidden first, every pass
   * measures the same true widths and converges on the same fit. */
  more.hidden = false;
  links.forEach((el) => { el.hidden = false; });
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

function paintPluginContext()       {
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
  const off = unavailable(page );
  const current_ = current ;
  /* The plugin's name lives here once (docs/39 S2): its rail glyph beside its label, so an
   * icon-only rail still names the destination and the title can never read as a tab. */
  title.hidden = false;
  title.title = off ? (off.lastError || tr("pageRegistry.pluginDisabled")) : "";
  fill(title, glyphNode(current_), h("span", { class: "ctx-name" }, tr(wireLabel(current_.label))));
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

function paintNavigation()       {
  paintPluginRail();
  paintPluginContext();
}

async function reloadPluginInventory()                                     {
  const response = await api("/api/plugins");
  if (response.status === 404) {
    inventory = null;
    registry.replace(legacy);
  } else {
    if (!response.ok) throw new Error(tr("pageRegistry.cannotLoadPluginInventory") + " HTTP " + response.status);
    const next                                                                                    = await response.json();
    const pages = next.pages || (next.plugins || []).flatMap((p) => { return p.pages || []; });
    registry.replace(pages.filter((p) => { return !["plugins", "secrets", "system"].includes(p.id); })
      .concat([management, vaultPage, systemPage]                   ));
    inventory = next;
  }
  paintNavigation();
  return inventory;
}

function initPages()                {
  if (boot) return boot;
  boot = (async () => {
    try { await reloadPluginInventory(); }
    catch (error) { toast(errText(error), true); paintNavigation(); }
    let wanted = (location.hash || "#mcps").replace(/^#\/?/, "");
    if (!registry.get(wanted)) wanted = registry.list()[0] && registry.list()[0].id;
    trackPaneScroll($("pane"));
    if (wanted) await navigatePage(wanted, true);
    window.addEventListener("hashchange", () => {
      const id = location.hash.replace(/^#\/?/, "");
      if (registry.get(id)) void navigatePage(id);
    });
    // Leaving focus mode dispatches a resize (immersive.ts) - the rail has width again.
    window.addEventListener("resize", scheduleRailRefit);
  })();
  return boot;
}

async function navigatePage(id        , force          )                {
  const page = registry.get(id);
  if (!page || (!force && active && active.id === id)) return;
  if (active && active.module.canLeave && !active.module.canLeave()) {
    history.replaceState(null, "", "#" + active.id);
    return;
  }
  const ticket = ++sequence;
  let module            ;
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
  document.querySelector             (".sidebar") .hidden = layoutOf(page) !== "resource";
  paintNavigation();
  history.replaceState(null, "", page.path || "#" + id);
  resetPaneScroll($("pane"));
  fill($("pane"), off
    ? h("div", { class: "empty" }, h("div", null,
        h("h2", null, tr("pageRegistry.pageUnavailable", { page: tr(wireLabel(page.label)) })),
        h("p", { class: "hint" }, off.lastError || tr("pageRegistry.pluginDisabledManagePlugins"))))
    : h("div", { class: "empty" }, tr("pageRegistry.loadingPage", { page: tr(wireLabel(page.label)) })));
  try { if (module.mount) await module.mount({ signal: controller.signal }); }
  catch (error) { if (ticket === sequence && error instanceof Error && error.name !== "AbortError") toast(errText(error), true); }
  if (ticket === sequence) $("countChip").textContent = currentPageCount();
}

async function pollPage()                {
  if (polling || !active || !active.module.poll) return;
  polling = true;
  try { await active.module.poll(); }
  catch (error) { toast(errText(error), true); }
  finally { polling = false; }
}
async function refreshPage()                {
  if (!active) return;
  try { if (active.module.refresh) await active.module.refresh(); else if (active.module.poll) await active.module.poll(); }
  catch (error) { toast(errText(error), true); }
}

export { currentPageCount, fitRailLabels, fitTabs, initPages, layoutOf, navigatePage, pageHasPendingChanges, pageMenuItems, pageUsesSidebar, pluginInventory, pollPage, railCaptionSize, RAIL_CAPTION_CEIL, RAIL_CAPTION_FLOOR, refreshPage, reloadPluginInventory };
