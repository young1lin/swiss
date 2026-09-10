import { $, api, esc, state, toast } from "./util.js";
import { createPageRegistry } from "./page-core.js";

/* Older gateways use this single manifest; a plugin-aware host supplies the same descriptors. */
var legacy = [
  { id: "mcps", pluginId: "mcp", label: "MCPs", sidebar: true },
  { id: "traffic", pluginId: "mcp", label: "Traffic" },
  { id: "tunnels", pluginId: "tunnels", label: "Tunnels" },
  { id: "data", pluginId: "data", label: "Data" },
  { id: "jobs", pluginId: "jobs", label: "Jobs" },
].map(function (p, i) { return Object.assign({ order: i * 10, path: "#" + p.id, entry: "/admin/js/views/" + p.id + ".js" }, p); });
var management = { id: "plugins", pluginId: "host", label: "Plugins", order: 1000, path: "#plugins", entry: "/admin/js/views/plugins.js" };
/* Group labels used when the host serves no plugin inventory (an older gateway answers 404 on
   /api/plugins), plus the one group that has no inventory row at all: the management page is
   synthesized here, not contributed by a plugin. */
var GROUP_LABELS = { mcp: "MCP", tunnels: "Tunnels", data: "Data", jobs: "Jobs", host: "Gateway" };
var registry = createPageRegistry();
registry.replace(legacy);
var inventory = null;
var active = null;
var sequence = 0;
var boot = null;
var polling = false;

function pluginInventory() { return inventory; }
function pageHasPendingChanges() { return !!(active && active.module.hasPendingChanges && active.module.hasPendingChanges()); }
function pageUsesSidebar() { var p = registry.get(state.view); return !!(p && p.sidebar); }
function currentPageCount() { return active && active.module.countText ? active.module.countText() : ""; }
function pluginFor(page) { return inventory && (inventory.plugins || []).find(function (p) { return p.id === page.pluginId; }); }
function unavailable(page) {
  var plugin = pluginFor(page);
  return plugin && (plugin.enabled === false || ["disabled", "failed", "waitingDependency", "not-built"].indexOf(plugin.state) >= 0) ? plugin : null;
}
/* Two-level navigation (docs/13): level one is one tab per plugin group, level two the current
   group's pages. Both bars delegate clicks on [data-view], and a level-one button also carries
   data-view (its group's default page) so the deep selector in jobs.js keeps matching. */
function pageTab(p) {
  var off = unavailable(p);
  return '<button role="tab" data-view="' + esc(p.id) + '" aria-selected="' + String(p.id === state.view) + '"' +
    (off ? ' title="' + esc(off.lastError || "Plugin disabled") + '"' : "") + ">" + esc(p.label) + (off ? " · off" : "") + "</button>";
}
function paintNavigation() {
  var groups = registry.groups(inventory && inventory.plugins, GROUP_LABELS);
  var current = groups.find(function (g) {
    return g.pages.some(function (p) { return p.id === state.view; });
  });
  var seg = $("viewSeg");
  seg.innerHTML = groups.map(function (g) {
    var allOff = g.pages.every(function (p) { return !!unavailable(p); });
    return '<button role="tab" data-group="' + esc(g.id) + '" data-view="' + esc(g.pages[0].id) +
      '" aria-selected="' + String(!!current && current.id === g.id) + '">' + esc(g.label) + (allOff ? " · off" : "") + "</button>";
  }).join("");
  seg.onclick = function (event) {
    var button = event.target.closest("[data-view]");
    if (button) void navigatePage(button.dataset.view);
  };
  var subBar = $("subBar");
  var subSeg = $("subSeg");
  if (current && current.pages.length >= 2) {
    subBar.hidden = false;
    subSeg.innerHTML = current.pages.map(pageTab).join("");
    subSeg.onclick = seg.onclick;
  } else {
    // One page per group is the norm (five of six today): a permanently visible bar of one tab
    // would be a permanent empty stripe, so the whole container leaves the layout instead (D5).
    subBar.hidden = true;
    subSeg.innerHTML = "";
  }
}

async function reloadPluginInventory() {
  var response = await api("/api/plugins");
  if (response.status === 404) {
    inventory = null;
    registry.replace(legacy);
  } else {
    if (!response.ok) throw new Error("Cannot load plugin inventory: HTTP " + response.status);
    var next = await response.json();
    var pages = next.pages || (next.plugins || []).flatMap(function (p) { return p.pages || []; });
    registry.replace(pages.filter(function (p) { return p.id !== "plugins"; }).concat([management]));
    inventory = next;
  }
  paintNavigation();
  return inventory;
}

function initPages() {
  if (boot) return boot;
  boot = (async function () {
    try { await reloadPluginInventory(); }
    catch (error) { toast(error.message, true); paintNavigation(); }
    var wanted = (location.hash || "#mcps").replace(/^#\/?/, "");
    if (!registry.get(wanted)) wanted = registry.list()[0] && registry.list()[0].id;
    if (wanted) await navigatePage(wanted, true);
    window.addEventListener("hashchange", function () {
      var id = location.hash.replace(/^#\/?/, "");
      if (registry.get(id)) void navigatePage(id);
    });
  })();
  return boot;
}

async function navigatePage(id, force) {
  var page = registry.get(id);
  if (!page || (!force && active && active.id === id)) return;
  if (active && active.module.canLeave && !active.module.canLeave()) {
    history.replaceState(null, "", "#" + active.id);
    return;
  }
  var ticket = ++sequence;
  var module;
  var off = unavailable(page);
  try { module = off ? {} : await registry.load(id); }
  catch (error) { toast(error.message, true); return; }
  if (ticket !== sequence) return;
  if (active) {
    active.controller.abort();
    if (active.module.unmount) await active.module.unmount();
  }
  var controller = new AbortController();
  state.view = id;
  active = { id: id, module: module, controller: controller };
  document.querySelector(".sidebar").hidden = !page.sidebar;
  paintNavigation();
  history.replaceState(null, "", page.path || "#" + id);
  $("pane").innerHTML = off
    ? '<div class="empty"><div><h2>' + esc(page.label) + ' unavailable</h2><p class="hint">' + esc(off.lastError || "This plugin is disabled. Manage it in Plugins.") + '</p></div></div>'
    : '<div class="empty">Loading ' + esc(page.label) + '…</div>';
  try { if (module.mount) await module.mount({ signal: controller.signal }); }
  catch (error) { if (ticket === sequence && error.name !== "AbortError") toast(error.message, true); }
  if (ticket === sequence) $("countChip").textContent = currentPageCount();
}

async function pollPage() {
  if (polling || !active || !active.module.poll) return;
  polling = true;
  try { await active.module.poll(); }
  catch (error) { toast(error.message, true); }
  finally { polling = false; }
}
async function refreshPage() {
  if (!active) return;
  try { if (active.module.refresh) await active.module.refresh(); else if (active.module.poll) await active.module.poll(); }
  catch (error) { toast(error.message, true); }
}

export { currentPageCount, initPages, navigatePage, pageHasPendingChanges, pageUsesSidebar, pluginInventory, pollPage, refreshPage, reloadPluginInventory };
