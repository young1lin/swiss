/* ================================================================================================
   Plugins — the host's own management view.

   Every other tab belongs to a plugin; this one belongs to the host, and that is why it is always
   reachable: a disabled plugin cannot serve the page that would switch it back on. An older
   gateway has no plugin host at all — page-registry.js only adds this tab once /api/plugins
   answers, so reaching mount() already means the inventory exists.

   Two writes, both revision-carrying: enable and disable send the revision the list was drawn
   from, so a second panel that changed something in between loses the race with a visible 409
   instead of silently overwriting it. A failed START is not a failed request — the row comes back
   state="failed" with its lastError, and the retry is pressing Enable again.
   ================================================================================================ */
import { $, apiJson, esc, toast } from "../util.js";
import { pluginInventory, reloadPluginInventory } from "../page-registry.js";

var busy = {}; // plugin id -> true while its own toggle is in flight
var painted = ""; // the structural signature of the drawn list; a change means rebuild

function inv() { return pluginInventory() || { plugins: [], revision: 0 }; }
function rows() { return inv().plugins || []; }
function signature() { return rows().map(function (p) { return p.id; }).join("\n"); }

function dotClass(p) {
  if (busy[p.id] || p.state === "starting") return "starting";
  if (p.state === "stopping") return "stopping";
  if (p.state === "failed") return "down";
  return p.enabled && p.state === "active" ? "up" : "idle";
}

/** What the row says it is doing, in the host's own words — never a guess of our own. */
function stateLabel(p) {
  if (busy[p.id]) return "working…";
  return p.enabled ? p.state : "disabled";
}

function rowHtml(p) {
  var pages = (p.pages || []).join(", ");
  return '<div class="tun-row" data-plugin="' + esc(p.id) + '">' +
      '<span class="dot ' + esc(dotClass(p)) + '" data-dot></span>' +
      '<div class="tun-main">' +
        '<div class="tun-name">' + esc(p.label || p.id) +
          (p.enabled ? "" : ' <span class="via">· off</span>') + "</div>" +
        '<div class="tun-sub"><code>' + esc(p.id) + "</code>" +
          ' <span class="via">· ' + esc(p.kind || "") + (p.version ? " v" + esc(p.version) : "") + "</span>" +
          ' <span class="via" data-state>· ' + esc(stateLabel(p)) + "</span>" +
          (pages ? ' <span class="via">· pages: ' + esc(pages) + "</span>" : ' <span class="via">· no page</span>') +
          '<span data-err>' + (p.lastError ? ' <span class="via">· ' + esc(p.lastError) + "</span>" : "") + "</span>" +
        "</div>" +
      "</div>" +
      '<div class="tun-acts">' +
        '<button class="btn" data-toggle' + (busy[p.id] ? " disabled" : "") + ">" +
          (busy[p.id] ? "…" : p.enabled ? "Disable" : "Enable") + "</button>" +
      "</div>" +
    "</div>";
}

function chipText() {
  var all = rows();
  var on = all.filter(function (p) { return p.enabled; }).length;
  var failed = all.filter(function (p) { return p.state === "failed"; }).length;
  return all.length + (all.length === 1 ? " plugin" : " plugins") + " · " + on + " on" +
    (failed ? " · " + failed + " failed" : "");
}

function render() {
  painted = signature();
  var all = rows();
  var body = all.length
    ? '<div class="group">' + all.map(rowHtml).join("") + "</div>"
    : '<div class="empty"><div><h2>No plugins</h2><p class="hint">This gateway reports an empty inventory.</p></div></div>';
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><h1 class="pane-title">Plugins</h1>' +
      '<div class="pane-desc">What this build is composed of. Disabling one stops its subsystem and takes its pages and API routes off the air until it is enabled again; definitions, logs and state files are left alone.</div>' +
    "</div></div>" +
    '<div class="sec-head"><span class="sec-cap">Installed</span>' +
      '<span class="via">revision ' + esc(String(inv().revision)) + "</span></div>" +
    body +
    '<div class="tun-foot" data-foot>' + esc(chipText()) + "</div>" +
  "</div>";
  wire();
}

/** Poll-safe update: dots, the state word, the error tail and the button. Never structure. */
function patch() {
  var pane = $("pane");
  if (!pane.querySelector(".group") || signature() !== painted) { render(); return; }
  Array.prototype.forEach.call(pane.querySelectorAll("[data-plugin]"), function (row) {
    var p = null;
    rows().forEach(function (cand) { if (cand.id === row.getAttribute("data-plugin")) p = cand; });
    if (!p) return;
    var dot = row.querySelector("[data-dot]");
    if (dot) dot.className = "dot " + dotClass(p);
    var st = row.querySelector("[data-state]");
    if (st) st.textContent = "· " + stateLabel(p);
    var err = row.querySelector("[data-err]");
    if (err) err.innerHTML = p.lastError ? ' <span class="via">· ' + esc(p.lastError) + "</span>" : "";
    var button = row.querySelector("[data-toggle]");
    if (button) {
      button.disabled = !!busy[p.id];
      button.textContent = busy[p.id] ? "…" : p.enabled ? "Disable" : "Enable";
    }
    var name = row.querySelector(".tun-name");
    if (name) name.innerHTML = esc(p.label || p.id) + (p.enabled ? "" : ' <span class="via">· off</span>');
  });
  var foot = pane.querySelector("[data-foot]");
  if (foot) foot.textContent = chipText();
}

function wire() {
  var pane = $("pane");
  pane.onclick = function (event) {
    var button = event.target.closest("[data-toggle]");
    if (!button) return;
    var row = button.closest("[data-plugin]");
    if (row) void toggle(row.getAttribute("data-plugin"));
  };
}

/** Enable or disable one plugin, then redraw from the host's answer — never from our guess of
 *  what the click should have done. The revision goes with the request so a stale list is
 *  refused rather than applied. */
async function toggle(id) {
  var p = null;
  rows().forEach(function (cand) { if (cand.id === id) p = cand; });
  if (!p || busy[id]) return;
  var action = p.enabled ? "disable" : "enable";
  busy[id] = true;
  patch();
  var reply = await apiJson("/api/plugins/" + encodeURIComponent(id) + "/" + action, {
    method: "POST",
    body: JSON.stringify({ revision: inv().revision }),
  });
  delete busy[id];
  // Reload either way: a refused write means our copy is stale, and a successful one changed
  // the page list the tab strip is drawn from.
  try { await reloadPluginInventory(); }
  catch (error) { /* reloadPluginInventory already toasted; keep the old rows on screen */ }
  render();
  // The enable stood and the START failed: a 200 whose row carries a lastError. The row already
  // shows it, but the click deserves an answer of its own — otherwise pressing Enable on a
  // plugin that cannot start looks like nothing happened.
  if (reply && reply.plugin && reply.plugin.lastError) toast(reply.plugin.lastError, true);
}

export async function mount() {
  busy = {};
  try { await reloadPluginInventory(); } catch (error) { /* the toast is enough; draw what we have */ }
  render();
}
export async function refresh() { await mount(); }
export async function poll() {
  try { await reloadPluginInventory(); } catch (error) { return; }
  patch();
}
export function countText() { return chipText(); }
export function unmount() { busy = {}; painted = ""; }
