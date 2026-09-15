/* ================================================================================================
   Secrets — the Gateway group's second page (docs/19 D6).

   The vault is host-owned: every plugin may depend on it, so it is not a plugin itself and
   lives beside Plugins, not inside it. Names only, ever (D5: write-only — a forgotten value
   is re-stored, never revealed); the value a row shows is the REFERENCE to copy into a
   credential field, not the credential. Writes carry the rev the list was drawn from, the
   same discipline Tokens and Plugins have.

   Groups (docs/20 G6): the list renders through the groups component with dragging off —
   secrets have no manual order, the name order IS the order (the scope's order route is a
   400). The store form carries a Group select; the header + preselects it. Group labels are
   folder names, not credentials — they are the one thing about a secret a listing may say
   beyond its name.
   ================================================================================================ */
import { $, apiJson, emptyHtml, esc, state, toast } from "../util.js";
import { copyText } from "../connect.js";
import { assignMember, groupOf as makeGroupOf, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, slice } from "../groups.js";

var painted = ""; // structural signature of the drawn list; a change means the rows move
var collapsed = {}; // the secrets fold map, loaded once before the first paint

/** The rendering group of one name: the stored label while its group lives, else the first
 *  group — the sink rule. An older gateway answers no groups at all: everything reads as
 *  the single default group, which the component draws as no divider at all. */
function groupOfName(name) {
  var g = secrets.memberGroups && secrets.memberGroups[name];
  return (secrets.groups || []).indexOf(g) >= 0 ? g : (secrets.groups || ["default"])[0];
}

function signature() {
  return (secrets.groups || []).join("\n") + "\u0000" +
    secrets.list.map(function (n) { return n + "\u0001" + groupOfName(n); }).join("\n");
}

/** The in-page copy of GET /api/secrets. On an older gateway the route 404s: apiJson toasts
 *  once and we draw the empty page — the same degradation every host page already has. */
var secrets = { list: [], rev: 0, loaded: false, groups: ["default"], memberGroups: {} };

async function loadSecrets() {
  var j = await apiJson("/api/secrets");
  if (!j) return;
  secrets.list = j.secrets || [];
  secrets.rev = j.rev || 0;
  secrets.groups = j.groups && j.groups.length ? j.groups : ["default"];
  secrets.memberGroups = j.secretGroups || {};
  secrets.loaded = true;
}

/** One secret's row. The buttons stay delegated on #pane (wire), so the groups component
 *  rebuilding a card never rewires them. */
function rowHtml(name) {
  return '<div class="row" data-secret="' + esc(name) + '"><div class="row-main">' +
    '<div class="name">' + esc(name) + "</div>" +
    '<div class="desc"><code>${secret://' + esc(name) + '}</code> — substituted at run time wherever a credential is used</div>' +
    "</div>" + '<div class="row-act">' +
      '<button class="btn" data-skcopy="' + esc(name) + '">Copy ref</button> ' +
      '<button class="btn danger" data-skdel="' + esc(name) + '">Delete</button>' +
    "</div></div>";
}

function rowsHtml() {
  return secrets.list.length ? secrets.list.map(rowHtml).join("") : "";
}

/** The secrets scope's cfg for mountGroup. No drag contract at all: rows sort by name, the
 *  order route is a 400 — the grip still reorders GROUPS (that is set_names, allowed), but
 *  nothing can drop a row anywhere. */
function skCfg() {
  return {
    scope: "secrets",
    density: "page",
    names: secrets.groups || ["default"],
    collapsed: collapsed,
    noun: "secret",
    addTitle: function (g) { return "Store a secret into " + g; },
    onAdd: function (g) {
      // The + points at the inline form: pick the group it names and put the cursor in the
      // name box — the sheet scopes open a modal; this page's flow was always inline.
      var sel = $("skGroup");
      if (sel) sel.value = g;
      $("skName").focus();
    },
    reload: function () { return loadSecrets().then(paintGroups); },
    render: paintGroups,
    draggable: false,
    rowsById: function () { return secrets.list; },
    groupOfRow: groupOfName,
    rowsHtml: function (g) { return g.rows.map(rowHtml).join(""); },
    rowSel: function (r) {
      var v = window.CSS && CSS.escape ? CSS.escape(r) : r;
      return '[data-secret="' + v + '"]';
    },
  };
}

/** (Re)build the groups region only — the store form lives outside it, so a repaint never
 *  wipes a half-typed value, the one place a value is ever typed. */
function paintGroups() {
  var host = $("skGroups");
  if (!host) return;
  painted = signature();
  host.innerHTML = "";
  if (!secrets.list.length) {
    host.innerHTML = emptyHtml({ icon: "key", title: "No secrets yet", hint: "Store a credential once, reference it everywhere as ${secret://name}." });
    return;
  }
  slice(secrets.list, secrets.groups || ["default"], groupOfName).forEach(function (g) {
    host.appendChild(mountGroup(skCfg(), g));
  });
  refreshGroupSelect();
  var chip = $("countChip");
  if (chip) chip.textContent = countText();
}

/** Poll-safe update: the groups region only, and only when the structure changed. */
function patch() {
  if (signature() === painted) return;
  paintGroups(); // repaints the rows AND the chip
}

/** Keep the form's Group select honest after a group mutation (docs/20 G6): a rename or
 *  delete that the poll reports must not leave a chosen-but-dead option in the box - the
 * next store would assign into a 400. The selection survives while its group lives; a dead
 * one falls back to the scope's last-used, else the first group. */
function refreshGroupSelect() {
  var sel = $("skGroup");
  if (!sel) return;
  var names = secrets.groups && secrets.groups.length ? secrets.groups : ["default"];
  var wanted = sel.value && names.indexOf(sel.value) >= 0
    ? sel.value
    : resolveDefaultGroup(names, lastGroup("secrets"));
  var next = names.map(function (n) {
    return '<option value="' + esc(n) + '"' + (n === wanted ? " selected" : "") + ">" + esc(n) + "</option>";
  }).join("");
  if (sel.innerHTML !== next) sel.innerHTML = next;
}

/** The Group select of the store form: the scope's groups, the last-used one selected. */
function groupSelectHtml() {
  var names = secrets.groups && secrets.groups.length ? secrets.groups : ["default"];
  var picked = resolveDefaultGroup(names, lastGroup("secrets"));
  return '<select class="v v-sk-group" id="skGroup" title="The group this secret lists under">' +
    names.map(function (n) {
      return '<option value="' + esc(n) + '"' + (n === picked ? " selected" : "") + ">" + esc(n) + "</option>";
    }).join("") + "</select>";
}

function render() {
  painted = signature();
  // No location title: the context bar already says "Gateway / Secrets".
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div>' +
      '<div class="pane-desc">Device-bound vault (docs/19). A value is written once and never shown again — not here, not in any API answer; a forgotten one can only be re-stored. Reference it wherever a credential goes: <code>${secret://name}</code> in a header, a URL, a command or an env value. A missing reference fails loudly at first use, naming where it was needed.</div>' +
    "</div></div>" +
    '<div class="vault-store">' +
      '<input class="v v-sk-name" id="skName" placeholder="name — lowercase kebab (a-z 0-9 -)">' +
      '<input class="v v-sk-value" id="skValue" type="password" placeholder="value — write-only, never shown again">' +
      groupSelectHtml() +
      '<button class="btn primary" id="skStore">Store</button>' +
      '<button class="btn" id="skNewGroup">New group</button>' +
    "</div>" +
    '<div id="skGroups"></div>' +
  "</div>";
  paintGroups();
  var chip = $("countChip");
  if (chip) chip.textContent = countText();
  wire();
}

function wire() {
  $("pane").onclick = async function (event) {
    var hit = event.target && event.target.closest ? event.target.closest("[data-skcopy],[data-skdel],#skStore,#skNewGroup") : null;
    if (!hit) return;
    if (hit.id === "skStore") { await storeSecret(); return; }
    if (hit.id === "skNewGroup") {
      newGroupFlow("secrets", secrets.groups || ["default"], function () {
        return loadSecrets().then(function () { render(); });
      });
      return;
    }
    if (hit.dataset.skcopy) { copyText("${secret://" + hit.dataset.skcopy + "}", "Reference"); return; }
    if (hit.dataset.skdel) { await removeSecret(hit.dataset.skdel); return; }
  };
}

/** Store one secret, then redraw from the vault's own answer. The rev goes with the write;
 *  a second panel that changed the vault in between loses the race with a visible 409.
 *  The selected group follows the name in one family write — an older gateway without the
 *  family keeps the first-group default, which is where the select's only option lands. */
async function storeSecret() {
  var name = ($("skName").value || "").trim();
  var value = $("skValue").value || "";
  if (!name || !value) { toast("a secret needs both a name and a value", true); return; }
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(name)) { toast("names are lowercase kebab: a-z, 0-9, dashes", true); return; }
  var picked = $("skGroup") ? $("skGroup").value : null;
  var j = await apiJson("/api/secrets/" + encodeURIComponent(name), {
    method: "PUT",
    body: JSON.stringify({ value: value, rev: secrets.rev }),
  });
  if (!j) return; // the toast said why; keep the form exactly as typed
  if (picked) {
    rememberGroup("secrets", picked);
    await assignMember("secrets", name, picked);
  }
  $("skValue").value = "";
  await loadSecrets();
  paintGroups();
  toast("stored — reference it as ${secret://" + name + "}");
}

/** Delete one secret after an explicit confirm: everything referencing it starts failing
 *  honestly until it is re-stored. */
async function removeSecret(name) {
  if (!confirm('Delete secret "' + name + '"? Everything referencing ${secret://' + name + "} starts failing until it is re-stored.")) return;
  var j = await apiJson("/api/secrets/" + encodeURIComponent(name) + "?rev=" + secrets.rev, { method: "DELETE" });
  if (!j) return;
  await loadSecrets();
  paintGroups();
  toast("deleted " + name);
}

export async function mount() {
  collapsed = loadCollapsed("secrets"); // before the first paint, so folded groups never flash open
  await loadSecrets();
  render();
}
export async function refresh() { await loadSecrets(); render(); }
export async function poll() { await loadSecrets(); patch(); }
export function countText() {
  var n = secrets.list.length;
  return n + (n === 1 ? " secret" : " secrets");
}
export function unmount() { painted = ""; }

/* Exported for the suite (the requiresBadge precedent): the rows markup and the two mutations,
 * so the contract is testable without a DOM that parses HTML. __setVaultForTest restores the
 * pre-first-load state between cases. */
export { rowsHtml, storeSecret, removeSecret };
export function __setVaultForTest(list, rev) { secrets = { list: list || [], rev: rev || 0, loaded: true, groups: ["default"], memberGroups: {} }; }