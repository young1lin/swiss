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
import { $, apiJson, emptyHtml, esc, toast } from "../util.js";
import { pluginInventory, reloadPluginInventory } from "../page-registry.js";

var busy = {}; // plugin id -> true while its own toggle is in flight
var painted = ""; // the structural signature of the drawn list; a change means rebuild

// The secret vault (docs/19): host-owned, like this whole page — every plugin may depend on it,
// so it is not a plugin itself. Names only; the gateway never hands a value back, so neither
// does this view. Writes carry the rev the list was drawn from, like every other editor here.
var secrets = { list: [], rev: 0, loaded: false };

function inv() { return pluginInventory() || { plugins: [], revision: 0 }; }
function rows() { return inv().plugins || []; }
function signature() {
  return rows().map(function (p) { return p.id; }).join("\n") + "\n§" + secrets.list.join("\n");
}

/** Fetch names + rev. Absent on an older gateway: the section simply never appears. */
async function loadSecrets() {
  var j = await apiJson("/api/secrets");
  if (!j) return;
  secrets.list = j.secrets || [];
  secrets.rev = j.rev || 0;
  secrets.loaded = true;
}

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
// (docs/18 V4): the state word left the row — the dot carries it; stateLabel is the dot's
// title (docs/18 V6), so the host's own vocabulary explains the colour on hover.

/** The dependency badge (docs/12 W3): a plugin whose required capability has no provider
 *  says SO on its row — "needs connection-catalog (no provider)" — instead of failing
 *  mysteriously when its floor is switched off. Met requirements render nothing, but the
 *  row always carries the (empty) span so poll-patch has its anchor either way.
 *  Exported pure for the suite: no DOM, just the row JSON in and badge HTML out. */
export function requiresBadge(p) {
  var missing = (p.requires || []).filter(function () { return p.requiresMet === false; });
  if (!missing.length) return "";
  return '· needs ' + esc(missing.join(", ")) + ' <span class="warn">(no provider)</span>';
}

/** One plugins row (docs/18 V4): dot + name + one grey line (id, pages, requirements,
 *  error) — version rides the row title, the state word is the dot's job — and the toggle
 *  is the panel's switch, the control every other row-level on/off uses. Exported pure. */
export function rowHtml(p) {
  var pages = (p.pages || []).join(", ");
  return '<div class="tun-row" data-plugin="' + esc(p.id) + '"' +
      (p.version ? ' title="v' + esc(p.version) + '"' : "") + ">" +
      '<span class="dot ' + esc(dotClass(p)) + '" data-dot title="' + esc(stateLabel(p)) + '"></span>' +
      '<div class="tun-main">' +
        '<div class="tun-name">' + esc(p.label || p.id) +
          (p.enabled ? "" : ' <span class="via">· off</span>') + "</div>" +
        '<div class="tun-sub"><code>' + esc(p.id) + "</code>" +
          (pages ? ' <span class="via">· pages: ' + esc(pages) + "</span>" : ' <span class="via">· no page</span>') +
          '<span class="via" data-reqs>' + requiresBadge(p) + "</span>" +
          '<span data-err>' + (p.lastError ? ' <span class="via">· ' + esc(p.lastError) + "</span>" : "") + "</span>" +
        "</div>" +
      "</div>" +
      '<div class="tun-acts">' +
        '<button class="sw" data-toggle role="switch" aria-checked="' + (p.enabled ? "true" : "false") + '"' +
          ' aria-label="Toggle ' + esc(p.label || p.id) + '"' + (busy[p.id] ? " disabled" : "") + "></button>" +
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
    : emptyHtml({ icon: "power", title: "No plugins", hint: "This gateway reports an empty inventory." });
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><h1 class="pane-title">Plugins</h1>' +
      '<div class="pane-desc">What this build is composed of. Disabling one stops its subsystem and takes its pages and API routes off the air until it is enabled again; definitions, logs and state files are left alone.</div>' +
    "</div></div>" +
    '<div class="sec-head"><span class="sec-cap">Installed</span></div>' +
    body +
    '<div class="tun-foot" data-foot><span data-foot-text>' + esc(chipText()) + "</span>" +
      '<span class="tun-foot-rev">revision ' + esc(String(inv().revision)) + "</span></div>" +
    secretsSection() +
  "</div>";
  wire();
}

/** The vault section (docs/19 D6): names, the reference to copy, a write-only form, delete.
 *  Hidden entirely on an older gateway (no /api/secrets) so the page never shows a dead form. */
function secretsSection() {
  if (!secrets.loaded) return "";
  var list = secrets.list.length
    ? '<div class="group">' + secrets.list.map(function (name) {
        return '<div class="tun-row" data-secret="' + esc(name) + '">' +
          '<span class="dot idle" data-dot title="stored"></span>' +
          '<div class="tun-main"><div class="tun-name">' + esc(name) + "</div>" +
            '<div class="tun-sub"><code>secret://' + esc(name) + "</code> — reference this wherever a credential is used; the value is substituted at run time.</div></div>" +
          '<button class="mini danger" data-secret-del aria-label="Delete ' + esc(name) + '">Delete</button>' +
        "</div>";
      }).join("") + "</div>"
    : emptyHtml({ icon: "power", title: "No secrets yet", hint: "Store a credential once, reference it everywhere as secret://name." });
  return '<div class="sec-head"><span class="sec-cap">Secrets</span></div>' +
    '<div class="pane-desc">Device-bound vault (docs/19). Values are written once and never shown again — a forgotten value can only be re-stored. Reference one anywhere a credential goes: <code>secret://name</code>. A missing reference fails loudly at first use.</div>' +
    list +
    '<div class="group" data-secret-form>' +
      '<div class="tun-row"><div class="tun-main">' +
        '<div class="tun-name">Store a secret</div>' +
        '<div class="tun-sub"><input class="fld" data-secret-name placeholder="name (lowercase-kebab)" style="width:200px"> ' +
          '<input class="fld" data-secret-value placeholder="value (write-only — never shown again)" type="password" style="width:300px"> ' +
          '<button class="sw" data-secret-save aria-label="Store the secret"></button></div>' +
      "</div></div>" +
    "</div>";
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
    // Class and title in one pass (docs/18 V6): the poll patches, never rebuilds, so the
    // title must follow the class or it keeps explaining the state before the last change.
    if (dot) { dot.className = "dot " + dotClass(p); dot.title = stateLabel(p); }
    var err = row.querySelector("[data-err]");
    if (err) err.innerHTML = p.lastError ? ' <span class="via">· ' + esc(p.lastError) + "</span>" : "";
    var reqs = row.querySelector("[data-reqs]");
    if (reqs) reqs.innerHTML = requiresBadge(p);
    var button = row.querySelector("[data-toggle]");
    if (button) {
      button.disabled = !!busy[p.id];
      button.setAttribute("aria-checked", p.enabled ? "true" : "false");
    }
    var name = row.querySelector(".tun-name");
    if (name) name.innerHTML = esc(p.label || p.id) + (p.enabled ? "" : ' <span class="via">· off</span>');
  });
  var foot = pane.querySelector("[data-foot-text]");
  if (foot) foot.textContent = chipText();
}

function wire() {
  var pane = $("pane");
  pane.onclick = function (event) {
    var del = event.target.closest("[data-secret-del]");
    if (del) {
      var row = del.closest("[data-secret]");
      if (row) void removeSecret(row.getAttribute("data-secret"));
      return;
    }
    if (event.target.closest("[data-secret-save]")) { void storeSecret(); return; }
    var button = event.target.closest("[data-toggle]");
    if (!button) return;
    var prow = button.closest("[data-plugin]");
    if (prow) void toggle(prow.getAttribute("data-plugin"));
  };
}

/** Store one secret, then redraw from the vault's own answer. The rev goes with the write, so
 *  a second panel that changed the vault in between loses the race with a visible error. */
async function storeSecret() {
  var pane = $("pane");
  var nameEl = pane.querySelector("[data-secret-name]");
  var valueEl = pane.querySelector("[data-secret-value]");
  var name = (nameEl && nameEl.value || "").trim();
  var value = valueEl && valueEl.value || "";
  if (!name || !value) { toast("a secret needs both a name and a value", true); return; }
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(name)) { toast("names are lowercase kebab: a-z, 0-9, dashes", true); return; }
  var j = await apiJson("/api/secrets/" + encodeURIComponent(name), {
    method: "PUT",
    body: JSON.stringify({ value: value, rev: secrets.rev }),
  });
  if (!j) return; // the toast said why; keep the form as typed
  if (valueEl) valueEl.value = "";
  await loadSecrets();
  render();
  toast("stored — reference it as secret://" + name);
}

/** Delete one secret after an explicit confirm. Runs using it start failing honestly. */
async function removeSecret(name) {
  if (!confirm('Delete secret "' + name + '"? Everything referencing secret://' + name + " starts failing until it is re-stored.")) return;
  var j = await apiJson("/api/secrets/" + encodeURIComponent(name) + "?rev=" + secrets.rev, { method: "DELETE" });
  if (!j) return;
  await loadSecrets();
  render();
  toast("deleted " + name);
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
  await loadSecrets();
  render();
}
export async function refresh() { await mount(); }
export async function poll() {
  try { await reloadPluginInventory(); } catch (error) { return; }
  patch();
}
export function countText() { return chipText(); }
// Exported pure for the suite (the requiresBadge precedent): the vault section's HTML, the
// loader, and the two mutations, so the contract is testable without a DOM that parses HTML.
// resetSecretsForTest exists only so a suite can observe the pre-first-load state again.
export { secretsSection, loadSecrets, storeSecret, removeSecret };
export function resetSecretsForTest() { secrets = { list: [], rev: 0, loaded: false }; }
export function unmount() { busy = {}; painted = ""; }
