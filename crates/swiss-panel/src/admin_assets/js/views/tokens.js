/* ================================================================================================
   Tokens — the MCP group's third page.

   The token exists FOR MCP clients: every copied connect command embeds one, and Traffic
   attributes requests by the token that carried them. That is why this page lives beside
   Servers and Traffic (the toolbar button is gone): manage the credential where the story
   that needs it is told.

   The view is poll-safe the way Jobs and Plugins are: the 6s poll refetches the list and
   patches the ROWS CONTAINER only — a full repaint would wipe the label input while
   somebody is typing a new token's name into it. The once-only secret box lives in
   state.tokenViewSecret and survives both poll and refresh until navigation leaves.

   Groups (docs/20 G7): the list renders through the groups component with dragging off —
   creation time is the tokens' order, so the scope's order route is a 400 and no row can be
   dropped anywhere. A group is a folder a token sits in; the "copies use this" marker is
   real state about the COPY actions, untouched by which folder the token sits in.
   ================================================================================================ */
import { $, TOKEN_ID_KEY, api, apiJson, emptyHtml, esc, state } from "../util.js";
import { claudeSnippet, copyText, fetchSecret, useToken } from "../connect.js";
import { assignMember, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, slice } from "../groups.js";

var painted = ""; // structural signature of the drawn list; a change means the rows move
var collapsed = {}; // the tokens fold map, loaded once before the first paint

/** Same rule as src/token.ts `pickCopyToken` — remembered id, else the `default` label, else first. */
function pickCopyToken(list, remembered) {
  if (!list.length) return null;
  if (remembered) {
    var hit = list.filter(function (t) { return t.id === remembered; })[0];
    if (hit) return hit;
  }
  return list.filter(function (t) { return t.label === "default"; })[0] || list[0] || null;
}

async function refreshTokens() {
  var r = await api("/api/tokens");
  if (!r.ok) return;
  var j = await r.json();
  state.tokens = j.tokens || [];
  // The two lists (docs/20 G7). An older gateway answers neither: the single default group,
  // which the component draws as no divider at all.
  state.tokenGroups = j.groups && j.groups.length ? j.groups : ["default"];
  state.tokenMembers = j.tokenGroups || {};
}

function rememberedTokenId() {
  try { return localStorage.getItem(TOKEN_ID_KEY); } catch (e) { return null; }
}

/** One `claude mcp add` line per MCP, embedding the given secret — a whole client setup in one copy. */
function connectAll(secret) {
  return (state.mcps || []).map(function (m) { return claudeSnippet(m.name, secret); }).join("\n");
}

/** The rendering group of one token: the stored label while its group lives, else the first
 *  group — the sink rule (docs/20 §2.1). */
function groupOfToken(t) {
  var g = state.tokenMembers && state.tokenMembers[t.id];
  return (state.tokenGroups || []).indexOf(g) >= 0 ? g : (state.tokenGroups || ["default"])[0];
}

function signature() {
  return (state.tokens || []).map(function (t) { return t.id + ":" + t.label + ":" + (t.createdAt || 0); }).join("\n") +
    "\u0000" + (state.tokenGroups || []).join("\n") + "\u0000" +
    (state.tokens || []).map(function (t) { return t.id + "\u0001" + groupOfToken(t); }).join("\n");
}

/** Which token the copy actions embed is real state the user should be able to see and change,
 *  rather than "whichever one you touched last". With no prior "Use", copies use `default`. */
function inUse() { return pickCopyToken(state.tokens || [], rememberedTokenId()); }

/** One token's row. The buttons stay delegated on #pane (wire), so the groups component
 *  rebuilding a card never rewires them. */
function rowHtml(t) {
  var mine = inUse();
  var used = mine && t.id === mine.id;
  return '<div class="row" data-token="' + esc(t.id) + '"><div class="row-main">' +
    '<div class="name">' + esc(t.label) + (used ? ' <span class="tag">copies use this</span>' : "") + "</div>" +
    '<div class="desc">id ' + esc(t.id) + (t.createdAt ? " · created " + new Date(t.createdAt).toLocaleString() : "") + "</div>" +
    '</div><div class="row-act">' +
      (used ? "" : '<button class="btn" data-tkuse="' + esc(t.id) + '">Use</button> ') +
      '<button class="btn" data-tkrot="' + esc(t.id) + '">Rotate</button> ' +
      '<button class="btn danger" data-tkdel="' + esc(t.id) + '">Revoke</button>' +
    "</div></div>";
}

function rowsHtml() {
  var list = state.tokens || [];
  return list.length ? list.map(rowHtml).join("") : '<div class="row"><span class="rowmsg">No tokens.</span></div>';
}

/** The tokens scope's cfg for mountGroup. No drag contract: creation time is the order, the
 *  order route is a 400 — the grip still reorders GROUPS (that is set_names, allowed), but
 *  nothing can drop a row anywhere. */
function tkCfg() {
  return {
    scope: "tokens",
    density: "page",
    names: state.tokenGroups || ["default"],
    collapsed: collapsed,
    noun: "token",
    addTitle: function (g) { return "Create a token in " + g; },
    onAdd: function (g) {
      // The + points at the inline form: pick the group it names and put the cursor in the
      // label box — the sheet scopes open a modal; this page's flow was always inline.
      var sel = $("tkGroup");
      if (sel) sel.value = g;
      $("tkLabel").focus();
    },
    reload: function () { return refreshTokens().then(paintGroups); },
    render: paintGroups,
    draggable: false,
    rowsById: function () { return state.tokens || []; },
    groupOfRow: groupOfToken,
    rowsHtml: function (g) { return g.rows.map(rowHtml).join(""); },
    rowSel: function (r) {
      var v = window.CSS && CSS.escape ? CSS.escape(r.id) : r.id;
      return '[data-token="' + v + '"]';
    },
  };
}

/** Keep the form's Group select honest after a group mutation (docs/20 G7): a rename or
 *  delete that the poll reports must not leave a chosen-but-dead option in the box — the
 *  next create would assign into a 400. The selection survives while its group lives. */
function refreshGroupSelect() {
  var sel = $("tkGroup");
  if (!sel) return;
  var names = state.tokenGroups && state.tokenGroups.length ? state.tokenGroups : ["default"];
  var wanted = sel.value && names.indexOf(sel.value) >= 0
    ? sel.value
    : resolveDefaultGroup(names, lastGroup("tokens"));
  var next = names.map(function (n) {
    return '<option value="' + esc(n) + '"' + (n === wanted ? " selected" : "") + ">" + esc(n) + "</option>";
  }).join("");
  if (sel.innerHTML !== next) sel.innerHTML = next;
}

/** (Re)build the groups region only — the create form and the secret box live outside it, so
 *  a repaint never wipes a half-typed label or the one chance to copy a secret. */
function paintGroups() {
  var host = $("tkGroups");
  if (!host) return;
  painted = signature();
  host.innerHTML = "";
  var list = state.tokens || [];
  if (!list.length) {
    host.innerHTML = emptyHtml({ icon: "key", title: "No tokens", hint: "One token per client — create one, then copy its connect command." });
    refreshGroupSelect();
    return;
  }
  slice(list, state.tokenGroups || ["default"], groupOfToken).forEach(function (g) {
    host.appendChild(mountGroup(tkCfg(), g));
  });
  refreshGroupSelect();
  $("countChip").textContent = countText();
}

/** The Group select of the create form: the scope's groups, the last-used one selected. */
function groupSelectHtml() {
  var names = state.tokenGroups && state.tokenGroups.length ? state.tokenGroups : ["default"];
  var picked = resolveDefaultGroup(names, lastGroup("tokens"));
  return '<select class="v" id="tkGroup" title="The group this token lists under">' +
    names.map(function (n) {
      return '<option value="' + esc(n) + '"' + (n === picked ? " selected" : "") + ">" + esc(n) + "</option>";
    }).join("") + "</select>";
}

/** The once-only secret box: shown after a create or rotate, kept in state so a poll or the
 *  explicit refresh cannot lose the one chance to copy it. */
function secretHtml() {
  var secret = state.tokenViewSecret;
  return secret
    ? '<div class="group" style="margin-top:var(--s4)"><div class="row"><div class="row-main">' +
        '<div class="name">New secret — copy now, shown only once</div>' +
        '<input class="v" style="width:100%" value="' + esc(secret) + '" readonly></div></div>' +
        '<div class="form-actions">' +
          '<button class="btn" id="tkCopySecret">Copy secret</button>' +
          '<button class="btn primary" id="tkCopyConn">Copy connect commands (all MCPs)</button>' +
        '</div></div>'
    : "";
}

function render() {
  painted = signature();
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div><h1 class="pane-title">Tokens</h1>' +
      '<div class="pane-desc">One token per client. Copied connect commands use the <code>default</code> token unless you click Use. The Traffic tab attributes every request to its token, and to the name the client announces during initialize. A secret is shown once — on create or rotate.</div>' +
    "</div></div>" +
    '<div class="two"><input id="tkLabel" placeholder="label, e.g. claude-code">' +
      groupSelectHtml() +
      '<button class="btn primary" id="tkCreate">Create</button>' +
      '<button class="btn" id="tkNewGroup">New group</button></div>' +
    '<div id="tkGroups"></div>' +
    secretHtml() +
  "</div>";
  paintGroups();
  $("countChip").textContent = countText();
  wire();
}

/** Poll-safe update: the groups region only, and only when the structure changed. The
 *  create form and the secret box are never touched from here. */
function patch() {
  if (signature() === painted) return;
  paintGroups(); // repaints the rows, the Group select's options and the chip
}

function wire() {
  $("pane").onclick = async function (event) {
    var button = event.target && event.target.closest ? event.target.closest("[data-tkuse],[data-tkrot],[data-tkdel],#tkCreate,#tkCopySecret,#tkCopyConn,#tkNewGroup") : null;
    if (!button) return;
    if (button.id === "tkCreate") {
      var j = await apiJson("/api/tokens", { method: "POST", body: JSON.stringify({ label: $("tkLabel").value }) });
      if (!j) return;
      // The selected group follows the new id in one family write; an older gateway without
      // the family keeps the first-group default (the select's only option).
      var picked = $("tkGroup") ? $("tkGroup").value : null;
      if (picked && j.id) {
        rememberGroup("tokens", picked);
        await assignMember("tokens", j.id, picked);
      }
      await refreshTokens();
      state.tokenViewSecret = j.secret;
      render();
      return;
    }
    if (button.id === "tkNewGroup") {
      newGroupFlow("tokens", state.tokenGroups || ["default"], function () {
        return refreshTokens().then(function () { render(); });
      });
      return;
    }
    if (button.id === "tkCopySecret") { copyText(state.tokenViewSecret, "Token secret"); return; }
    if (button.id === "tkCopyConn") { copyText(connectAll(state.tokenViewSecret), "Connect commands"); return; }
    if (button.dataset.tkuse) {
      if (await fetchSecret(button.dataset.tkuse)) render();
      return;
    }
    if (button.dataset.tkrot) {
      var r = await apiJson("/api/tokens/" + encodeURIComponent(button.dataset.tkrot) + "/rotate", { method: "POST" });
      if (!r) return;
      await refreshTokens();
      state.tokenViewSecret = r.secret;
      if (rememberedTokenId() === r.id) useToken(r.id, r.secret);
      render();
      return;
    }
    if (button.dataset.tkdel) {
      if (!confirm("Revoke this token? Clients using it stop working immediately.")) return;
      var id = button.dataset.tkdel;
      var d = await apiJson("/api/tokens/" + encodeURIComponent(id), { method: "DELETE" });
      if (!d) return;
      if (rememberedTokenId() === id) {
        state.activeSecret = null;
        try { localStorage.removeItem(TOKEN_ID_KEY); } catch (e) { /* blocked */ }
      }
      await refreshTokens();
      render();
    }
  };
}

export async function mount() {
  state.tokenViewSecret = null; // the once-only box is per-visit, like the sheet it replaced
  collapsed = loadCollapsed("tokens"); // before the first paint, so folded groups never flash open
  await refreshTokens();
  render();
}
export async function refresh() { await refreshTokens(); render(); }
export async function poll() { await refreshTokens(); patch(); }
export function countText() {
  var n = (state.tokens || []).length;
  return n + (n === 1 ? " token" : " tokens");
}
export function unmount() { painted = ""; }

export { pickCopyToken, refreshTokens, rememberedTokenId };