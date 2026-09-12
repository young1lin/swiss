/* ================================================================================================
   Secrets — the Gateway group's second page (docs/19 D6).

   The vault is host-owned: every plugin may depend on it, so it is not a plugin itself and
   lives beside Plugins, not inside it. Names only, ever (D5: write-only — a forgotten value
   is re-stored, never revealed); the value a row shows is the REFERENCE to copy into a
   credential field, not the credential. Writes carry the rev the list was drawn from, the
   same discipline Tokens and Plugins have.
   ================================================================================================ */
import { $, apiJson, emptyHtml, esc, toast } from "../util.js";
import { copyText } from "../connect.js";

var painted = ""; // structural signature of the drawn list; a change means the rows move

function signature() { return secrets.list.join("\n"); }

/** The in-page copy of GET /api/secrets. On an older gateway the route 404s: apiJson toasts
 *  once and we draw the empty page — the same degradation every host page already has. */
var secrets = { list: [], rev: 0, loaded: false };

async function loadSecrets() {
  var j = await apiJson("/api/secrets");
  if (!j) return;
  secrets.list = j.secrets || [];
  secrets.rev = j.rev || 0;
  secrets.loaded = true;
}

function rowsHtml() {
  return secrets.list.length
    ? secrets.list.map(function (name) {
        return '<div class="row"><div class="row-main">' +
          '<div class="name">' + esc(name) + "</div>" +
          '<div class="desc"><code>secret://' + esc(name) + "</code> — substituted at run time wherever a credential is used</div>" +
          "</div>" + '<div class="row-act">' +
            '<button class="btn" data-skcopy="' + esc(name) + '">Copy ref</button> ' +
            '<button class="btn danger" data-skdel="' + esc(name) + '">Delete</button>' +
          "</div></div>";
      }).join("")
    : '<div class="row"><span class="rowmsg">No secrets yet. Store a credential once, reference it everywhere as <code>secret://name</code>.</span></div>';
}

/** Poll-safe update: the rows container only, and only when the list changed — a repaint
 *  mid-typing would wipe the store form, the one place a value is ever typed. */
function patch() {
  if (signature() === painted) return;
  painted = signature();
  var rows = $("pane").querySelector("[data-rows]");
  if (rows) rows.innerHTML = rowsHtml();
  var chip = $("countChip");
  if (chip) chip.textContent = countText();
}

function render() {
  painted = signature();
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><h1 class="pane-title">Secrets</h1>' +
      '<div class="pane-desc">Device-bound vault (docs/19). A value is written once and never shown again — not here, not in any API answer; a forgotten one can only be re-stored. Reference it wherever a credential goes: <code>secret://name</code> in a header, a URL, a command or an env value. A missing reference fails loudly at first use, naming where it was needed.</div>' +
    "</div></div>" +
    '<div class="vault-store">' +
      '<input class="v v-sk-name" id="skName" placeholder="name — lowercase kebab (a-z 0-9 -)">' +
      '<input class="v v-sk-value" id="skValue" type="password" placeholder="value — write-only, never shown again">' +
      '<button class="btn primary" id="skStore">Store</button>' +
    "</div>" +
    '<div class="group" data-rows="">' + rowsHtml() + "</div>" +
  "</div>";
  var chip = $("countChip");
  if (chip) chip.textContent = countText();
}

function wire() {
  $("pane").onclick = async function (event) {
    var hit = event.target && event.target.closest ? event.target.closest("[data-skcopy],[data-skdel],#skStore") : null;
    if (!hit) return;
    if (hit.id === "skStore") { await storeSecret(); return; }
    if (hit.dataset.skcopy) { copyText("secret://" + hit.dataset.skcopy, "Reference"); return; }
    if (hit.dataset.skdel) { await removeSecret(hit.dataset.skdel); return; }
  };
}

/** Store one secret, then redraw from the vault's own answer. The rev goes with the write;
 *  a second panel that changed the vault in between loses the race with a visible 409. */
async function storeSecret() {
  var name = ($("skName").value || "").trim();
  var value = $("skValue").value || "";
  if (!name || !value) { toast("a secret needs both a name and a value", true); return; }
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(name)) { toast("names are lowercase kebab: a-z, 0-9, dashes", true); return; }
  var j = await apiJson("/api/secrets/" + encodeURIComponent(name), {
    method: "PUT",
    body: JSON.stringify({ value: value, rev: secrets.rev }),
  });
  if (!j) return; // the toast said why; keep the form exactly as typed
  $("skValue").value = "";
  await loadSecrets();
  render();
  toast("stored — reference it as secret://" + name);
}

/** Delete one secret after an explicit confirm: everything referencing it starts failing
 *  honestly until it is re-stored. */
async function removeSecret(name) {
  if (!confirm('Delete secret "' + name + '"? Everything referencing secret://' + name + " starts failing until it is re-stored.")) return;
  var j = await apiJson("/api/secrets/" + encodeURIComponent(name) + "?rev=" + secrets.rev, { method: "DELETE" });
  if (!j) return;
  await loadSecrets();
  render();
  toast("deleted " + name);
}

export async function mount() { await loadSecrets(); render(); wire(); }
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
export function __setVaultForTest(list, rev) { secrets = { list: list || [], rev: rev || 0, loaded: true }; }
