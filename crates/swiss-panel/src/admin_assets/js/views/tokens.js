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
   ================================================================================================ */
import { $, TOKEN_ID_KEY, api, apiJson, esc, state } from "../util.js";
import { claudeSnippet, copyText, fetchSecret, useToken } from "../connect.js";

var painted = ""; // structural signature of the drawn list; a change means the rows move

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
  if (r.ok) state.tokens = (await r.json()).tokens || [];
}

function rememberedTokenId() {
  try { return localStorage.getItem(TOKEN_ID_KEY); } catch (e) { return null; }
}

/** One `claude mcp add` line per MCP, embedding the given secret — a whole client setup in one copy. */
function connectAll(secret) {
  return (state.mcps || []).map(function (m) { return claudeSnippet(m.name, secret); }).join("\n");
}

function signature() {
  return (state.tokens || []).map(function (t) { return t.id + ":" + t.label + ":" + (t.createdAt || 0); }).join("\n");
}

/** Which token the copy actions embed is real state the user should be able to see and change,
 *  rather than "whichever one you touched last". With no prior "Use", copies use `default`. */
function inUse() { return pickCopyToken(state.tokens || [], rememberedTokenId()); }

function rowsHtml() {
  var mine = inUse();
  var list = state.tokens || [];
  return list.length ? list.map(function (t) {
    var used = mine && t.id === mine.id;
    return '<div class="row"><div class="row-main">' +
      '<div class="name">' + esc(t.label) + (used ? ' <span class="tag">copies use this</span>' : '') + '</div>' +
      '<div class="desc">id ' + esc(t.id) + (t.createdAt ? ' · created ' + new Date(t.createdAt).toLocaleString() : '') + '</div>' +
      '</div><div class="row-act">' +
        (used ? '' : '<button class="btn" data-tkuse="' + esc(t.id) + '">Use</button> ') +
        '<button class="btn" data-tkrot="' + esc(t.id) + '">Rotate</button> ' +
        '<button class="btn danger" data-tkdel="' + esc(t.id) + '">Revoke</button>' +
      '</div></div>';
  }).join("") : '<div class="row"><span class="rowmsg">No tokens.</span></div>';
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
    '<button class="btn primary" id="tkCreate">Create</button></div>' +
    '<div class="group" data-rows="">' + rowsHtml() + "</div>" +
    secretHtml() +
  "</div>";
  $("countChip").textContent = countText();
}

/** Poll-safe update: the rows container only, and only when the list actually changed. The
 *  create form and the secret box are never touched from here. */
function patch() {
  if (signature() === painted) return;
  painted = signature();
  var rows = $("pane").querySelector("[data-rows]");
  if (rows) rows.innerHTML = rowsHtml();
  $("countChip").textContent = countText();
}

function wire() {
  $("pane").onclick = async function (event) {
    var button = event.target && event.target.closest ? event.target.closest("[data-tkuse],[data-tkrot],[data-tkdel],#tkCreate,#tkCopySecret,#tkCopyConn") : null;
    if (!button) return;
    if (button.id === "tkCreate") {
      var j = await apiJson("/api/tokens", { method: "POST", body: JSON.stringify({ label: $("tkLabel").value }) });
      if (!j) return;
      await refreshTokens();
      state.tokenViewSecret = j.secret;
      render();
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
  await refreshTokens();
  render();
  wire();
}
export async function refresh() { await refreshTokens(); render(); }
export async function poll() { await refreshTokens(); patch(); }
export function countText() {
  var n = (state.tokens || []).length;
  return n + (n === 1 ? " token" : " tokens");
}
export function unmount() { painted = ""; }

export { pickCopyToken, refreshTokens, rememberedTokenId };