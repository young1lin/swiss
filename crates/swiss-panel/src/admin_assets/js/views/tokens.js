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
import { $, TOKEN_ID_KEY, api, apiJson, emptyHtml, esc, icon, state, targetEl } from "../util.js";
import { claudeSnippet, copyText, fetchSecret, useToken } from "../connect.js";
import { popupMenu } from "../menu.js";
import { assignMember, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, slice } from "../groups.js";

let painted = ""; // structural signature of the drawn list; a change means the rows move
let collapsed                          = {}; // the tokens fold map, loaded once before the first paint

/** Same rule as src/token.ts `pickCopyToken` — remembered id, else the `default` label, else first. */
function pickCopyToken(list               , remembered               )                     {
  if (!list.length) return null;
  if (remembered) {
    const hit = list.filter((t             )          => { return t.id === remembered; })[0];
    if (hit) return hit;
  }
  return list.filter((t             )          => { return t.label === "default"; })[0] || list[0] || null;
}

async function refreshTokens()                {
  const r = await api("/api/tokens");
  if (!r.ok) return;
  const j = await r.json()                     ;
  state.tokens = j.tokens || [];
  // The two lists (docs/20 G7). An older gateway answers neither: the single default group,
  // which the component draws as no divider at all.
  state.tokenGroups = j.groups && j.groups.length ? j.groups : ["default"];
  state.tokenMembers = j.tokenGroups || {};
}

function rememberedTokenId()                {
  try { return localStorage.getItem(TOKEN_ID_KEY); } catch (e) { return null; }
}

/** One `claude mcp add` line per MCP, embedding the given secret — a whole client setup in one copy. */
function connectAll(secret        )         {
  return (state.mcps || []).map((m           )         => { return claudeSnippet(m.name, secret); }).join("\n");
}

/** The rendering group of one token: the stored label while its group lives, else the first
 *  group — the sink rule (docs/20 §2.1). */
function groupOfToken(t             )         {
  const g = state.tokenMembers && state.tokenMembers[t.id];
  return (state.tokenGroups || []).indexOf(g ) >= 0 ? g  : (state.tokenGroups || ["default"])[0];
}

function signature()         {
  return (state.tokens || []).map((t             )         => { return t.id + ":" + t.label + ":" + (t.createdAt || 0); }).join("\n") +
    "\u0000" + (state.tokenGroups || []).join("\n") + "\u0000" +
    (state.tokens || []).map((t             )         => { return t.id + "\u0001" + groupOfToken(t); }).join("\n");
}

/** Which token the copy actions embed is real state the user should be able to see and change,
 *  rather than "whichever one you touched last". With no prior "Use", copies use `default`. */
function inUse()                     { return pickCopyToken(state.tokens || [], rememberedTokenId()); }

/** One token's row: one button (Use, absent on the token copies already use) and the
 *  overflow menu with Rotate and Revoke - red never sits on a row (design rule 4). The
 *  buttons stay delegated on #pane (wire), so the groups component rebuilding a card never
 *  rewires them. */
function rowHtml(t             )         {
  const mine = inUse();
  const used = mine && t.id === mine.id;
  return '<div class="row" data-token="' + esc(t.id) + '"><div class="row-main">' +
    '<div class="name">' + esc(t.label) + (used ? ' <span class="tag">copies use this</span>' : "") + "</div>" +
    '<div class="desc">id ' + esc(t.id) + (t.createdAt ? " · created " + new Date(t.createdAt).toLocaleString() : "") + "</div>" +
    '</div><div class="row-act">' +
      (used ? "" : '<button class="btn" data-tkuse="' + esc(t.id) + '">Use</button> ') +
      '<button class="btn ghost icon" data-tkmore="' + esc(t.id) + '" aria-label="Actions for ' + esc(t.label) + '" title="Rotate or revoke">' + icon("ellipsis") + "</button>" +
    "</div></div>";
}

function rowsHtml()         {
  const list = state.tokens || [];
  return list.length ? list.map(rowHtml).join("") : '<div class="row"><span class="rowmsg">No tokens.</span></div>';
}

/** The tokens scope's cfg for mountGroup. No drag contract: creation time is the order, the
 *  order route is a 400 — the grip still reorders GROUPS (that is set_names, allowed), but
 *  nothing can drop a row anywhere. */
function tkCfg()                        {
  return {
    scope: "tokens",
    density: "page",
    names: state.tokenGroups || ["default"],
    collapsed: collapsed,
    noun: "token",
    addTitle: (g        )         => { return "Create a token in " + g; },
    onAdd: (g        )       => {
      // The + points at the inline form: pick the group it names and put the cursor in the
      // label box — the sheet scopes open a modal; this page's flow was always inline.
      const sel = $                   ("tkGroup");
      if (sel) sel.value = g;
      $                  ("tkLabel").focus();
    },
    reload: ()                => { return refreshTokens().then(paintGroups); },
    render: paintGroups,
    draggable: false,
    rowsById: ()                => { return state.tokens || []; },
    groupOfRow: groupOfToken,
    rowsHtml: (g                         )         => { return g.rows.map(rowHtml).join(""); },
    rowSel: (r             )         => {
      const v = window.CSS && CSS.escape ? CSS.escape(r.id) : r.id;
      return '[data-token="' + v + '"]';
    },
  };
}

/** Keep the form's Group select honest after a group mutation (docs/20 G7): a rename or
 *  delete that the poll reports must not leave a chosen-but-dead option in the box — the
 *  next create would assign into a 400. The selection survives while its group lives. */
function refreshGroupSelect()       {
  const sel = $                   ("tkGroup");
  if (!sel) return;
  const names = state.tokenGroups && state.tokenGroups.length ? state.tokenGroups : ["default"];
  const wanted = sel.value && names.indexOf(sel.value) >= 0
    ? sel.value
    : resolveDefaultGroup(names, lastGroup("tokens"));
  const next = names.map((n        )         => {
    return '<option value="' + esc(n) + '"' + (n === wanted ? " selected" : "") + ">" + esc(n) + "</option>";
  }).join("");
  if (sel.innerHTML !== next) sel.innerHTML = next;
}

/** (Re)build the groups region only — the create form and the secret box live outside it, so
 *  a repaint never wipes a half-typed label or the one chance to copy a secret. */
function paintGroups()       {
  const host = $("tkGroups");
  if (!host) return;
  painted = signature();
  host.innerHTML = "";
  const list = state.tokens || [];
  if (!list.length) {
    host.innerHTML = emptyHtml({ icon: "key", title: "No tokens", hint: "One token per client — create one, then copy its connect command." });
    refreshGroupSelect();
    return;
  }
  slice(list, state.tokenGroups || ["default"], groupOfToken).forEach((g                         )       => {
    host.appendChild(mountGroup(tkCfg(), g));
  });
  refreshGroupSelect();
  $("countChip").textContent = countText();
}

/** The Group select of the create form: the scope's groups, the last-used one selected. */
function groupSelectHtml()         {
  const names = state.tokenGroups && state.tokenGroups.length ? state.tokenGroups : ["default"];
  const picked = resolveDefaultGroup(names, lastGroup("tokens"));
  return '<select class="v" id="tkGroup" title="The group this token lists under">' +
    names.map((n        )         => {
      return '<option value="' + esc(n) + '"' + (n === picked ? " selected" : "") + ">" + esc(n) + "</option>";
    }).join("") + "</select>";
}

/** The once-only secret box: shown after a create or rotate, kept in state so a poll or the
 *  explicit refresh cannot lose the one chance to copy it. */
function secretHtml()         {
  const secret = state.tokenViewSecret;
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

function render()       {
  painted = signature();
  // No location title: the context bar already says "MCP / Token".
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div>' +
      '<div class="pane-desc">One token per client. Copied connect commands use the <code>default</code> token unless you click Use. The Traffic tab attributes every request to its token, and to the name the client announces during initialize. A secret is shown once — on create or rotate.</div>' +
    "</div>" +
      '<div class="pane-actions"><button class="btn" id="tkNewGroup">New group</button></div>' +
    "</div>" +
    // The inline create form (docs/35 §3): one row, the Group select beside the primary.
    '<div class="inline-form"><input id="tkLabel" placeholder="Label, e.g. claude-code">' +
      groupSelectHtml() +
      '<button class="btn primary" id="tkCreate">Create</button></div>' +
    '<div id="tkGroups"></div>' +
    secretHtml() +
  "</div>";
  paintGroups();
  $("countChip").textContent = countText();
  wire();
}

/** Poll-safe update: the groups region only, and only when the structure changed. The
 *  create form and the secret box are never touched from here. */
function patch()       {
  if (signature() === painted) return;
  paintGroups(); // repaints the rows, the Group select's options and the chip
}

/** Rotate: a new secret at once (the old one stops working), shown in the once-only box. */
async function rotateToken(id        )                {
  const r = await apiJson                 ("/api/tokens/" + encodeURIComponent(id) + "/rotate", { method: "POST" });
  if (!r) return;
  await refreshTokens();
  state.tokenViewSecret = r.secret;
  if (rememberedTokenId() === r.id) useToken(r.id, r.secret);
  render();
}

/** Revoke after an explicit confirm; a revoked in-use token also clears the copy choice. */
async function revokeToken(id        )                {
  if (!confirm("Revoke this token? Clients using it stop working immediately.")) return;
  const d = await apiJson         ("/api/tokens/" + encodeURIComponent(id), { method: "DELETE" });
  if (!d) return;
  if (rememberedTokenId() === id) {
    state.activeSecret = null;
    try { localStorage.removeItem(TOKEN_ID_KEY); } catch (e) { /* blocked */ }
  }
  await refreshTokens();
  render();
}

function wire()       {
  $("pane").onclick = async (event            )                => {
    const button = targetEl(event)?.closest             ("[data-tkuse],[data-tkmore],#tkCreate,#tkCopySecret,#tkCopyConn,#tkNewGroup");
    if (!button) return;
    if (button.id === "tkCreate") {
      const j = await apiJson                 ("/api/tokens", { method: "POST", body: JSON.stringify({ label: $                  ("tkLabel").value }) });
      if (!j) return;
      // The selected group follows the new id in one family write; an older gateway without
      // the family keeps the first-group default (the select's only option).
      const picked = $("tkGroup") ? $                   ("tkGroup").value : null;
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
      newGroupFlow("tokens", state.tokenGroups || ["default"], ()                => {
        return refreshTokens().then(()       => { render(); });
      });
      return;
    }
    if (button.id === "tkCopySecret") { copyText(state.tokenViewSecret , "Token secret"); return; }
    if (button.id === "tkCopyConn") { copyText(connectAll(state.tokenViewSecret ), "Connect commands"); return; }
    if (button.dataset.tkuse) {
      if (await fetchSecret(button.dataset.tkuse)) render();
      return;
    }
    if (button.dataset.tkmore) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      const id = button.dataset.tkmore;
      popupMenu(button.getBoundingClientRect(), [
        { label: "Rotate secret", fn: ()       => { void rotateToken(id ); } },
        { sep: true },
        { label: "Revoke", danger: true, fn: ()       => { void revokeToken(id ); } },
      ]);
    }
  };
}

export async function mount()                {
  state.tokenViewSecret = null; // the once-only box is per-visit, like the sheet it replaced
  collapsed = loadCollapsed("tokens"); // before the first paint, so folded groups never flash open
  await refreshTokens();
  render();
}
export async function refresh()                { await refreshTokens(); render(); }
export async function poll()                { await refreshTokens(); patch(); }
export function countText()         {
  const n = (state.tokens || []).length;
  return n + (n === 1 ? " token" : " tokens");
}
export function unmount()       { painted = ""; }

export { pickCopyToken, refreshTokens, rememberedTokenId };