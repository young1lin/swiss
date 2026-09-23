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

/* ================================================================================================
   Tokens — the MCP group's third page.

   The token exists FOR MCP clients: every copied connect command embeds one, and Traffic
   attributes requests by the token that carried them. That is why this page lives beside
   Servers and Traffic (the toolbar button is gone): manage the credential where the story
   that needs it is told.

   The view is poll-safe the way Jobs and Plugins are: the 6s poll refetches the list and
   patches the ROWS CONTAINER only — a full repaint would wipe the label input while
   somebody is typing a new token's name into it. The once-only secret box lives in
   the tokens domain (setTokenViewSecret) and survives both poll and refresh until navigation leaves.

   Groups (docs/20 G7): the list renders through the groups component with dragging off —
   creation time is the tokens' order, so the scope's order route is a 400 and no row can be
   dropped anywhere. A group is a folder a token sits in; the "copies use this" marker is
   real state about the COPY actions, untouched by which folder the token sits in.
   ================================================================================================ */
import type { ApiMcpRow, ApiTokenCreated, ApiTokenRow, ApiTokensResponse } from "../types/api.js";
import type { GroupCfg, GroupSlice } from "../types/dom.js";
import { $, TOKEN_ID_KEY, api, apiJson, lsMigrate, targetEl } from "../util.js";
import { fill, h } from "../h.js";
import { claudeSnippet, copyText, fetchSecret, useToken } from "../connect.js";
import { assignMember, lastGroup, loadCollapsed, mountGroup, newGroupFlow, rememberGroup, resolveDefaultGroup, slice } from "../groups.js";
import { mcpRows } from "../mcp-state.js";
import { locale, tr, trn } from "../i18n.js";
import {
  btn, card, emptyNode, field, form, formActions, iconBtn, inlineForm, moreBtn, paneBody, paneHead, popupMenu, row, section, tag,
} from "../ui/index.js";

let painted = ""; // structural signature of the drawn list; a change means the rows move
let collapsed: Record<string, boolean> = {}; // the tokens fold map, loaded once before the first paint

/* The tokens domain's own state (docs/37 R4, slice 1 of 7): the named-token list, its
 * group family, the most-recently revealed secret, and the once-only create/rotate box.
 * Everything reads and writes through the accessors below - no other module reaches in
 * (connect.ts resolves through them, main.ts boots through loadTokens). */
const tokensDomain: {
  list: ApiTokenRow[];
  groups: string[];
  members: Record<string, string>;
  activeSecret: string | null;
  viewSecret: string | null;
} = { list: [], groups: [], members: {}, activeSecret: null, viewSecret: null };

export function tokenRows(): ApiTokenRow[] { return tokensDomain.list; }
export function setTokenRows(rows: ApiTokenRow[]): void { tokensDomain.list = rows; }
export function tokenGroupNames(): string[] { return tokensDomain.groups; }
export function tokenMemberOf(id: string): string | undefined { return tokensDomain.members[id]; }
export function setTokenFamily(groups: string[], members: Record<string, string>): void {
  tokensDomain.groups = groups;
  tokensDomain.members = members;
}
export function activeTokenSecret(): string | null { return tokensDomain.activeSecret; }
/** The once-only box's secret - test seams and rotate/create set it; mount clears it. */
export function setTokenViewSecret(secret: string | null): void { tokensDomain.viewSecret = secret; }
export function setActiveTokenSecret(secret: string | null): void { tokensDomain.activeSecret = secret; }

/** The tokens domain's load (the old util-state boot and connect.ts both funnel here):
 * /api/tokens, then the group family on top. */
export async function loadTokens(): Promise<void> {
  const r = await api("/api/tokens");
  if (!r.ok) return;
  const j = await r.json() as ApiTokensResponse;
  tokensDomain.list = j.tokens || [];
  // The two lists (docs/20 G7). An older gateway answers neither: the single default group,
  // which the component draws as no divider at all.
  tokensDomain.groups = j.groups && j.groups.length ? j.groups : ["default"];
  tokensDomain.members = j.tokenGroups || {};
}

/** Same rule as src/token.ts `pickCopyToken` — remembered id, else the `default` label, else first. */
function pickCopyToken(list: ApiTokenRow[], remembered: string | null): ApiTokenRow | null {
  if (!list.length) return null;
  if (remembered) {
    const hit = list.find((t: ApiTokenRow): boolean => { return t.id === remembered; });
    if (hit) return hit;
  }
  return list.find((t: ApiTokenRow): boolean => { return t.label === "default"; }) || list[0] || null;
}

async function refreshTokens(): Promise<void> {
  await loadTokens();
}

function rememberedTokenId(): string | null {
  // fix-plan #17: one-shot migration off the mcp_gateway_token_id era; a fresh browser
  // only ever answers - and later writes - the swiss.tokenId slot.
  return lsMigrate(TOKEN_ID_KEY, "mcp_gateway_token_id");
}

/** One `claude mcp add` line per MCP, embedding the given secret — a whole client setup in one copy. */
function connectAll(secret: string): string {
  return (mcpRows() || []).map((m: ApiMcpRow): string => { return claudeSnippet(m.name, secret); }).join("\n");
}

/** The rendering group of one token: the stored label while its group lives, else the first
 *  group — the sink rule (docs/20 §2.1). */
function groupOfToken(t: ApiTokenRow): string {
  const g = tokenMemberOf(t.id);
  const names = tokenGroupNames();
  return names.includes(g!) ? g! : (names.length ? names[0] : "default");
}

function signature(): string {
  return tokenRows().map((t: ApiTokenRow): string => { return t.id + ":" + t.label + ":" + (t.createdAt || 0); }).join("\n") +
    "\u0000" + tokenGroupNames().join("\n") + "\u0000" +
    tokenRows().map((t: ApiTokenRow): string => { return t.id + "\u0001" + groupOfToken(t); }).join("\n");
}

/** Which token the copy actions embed is real state the user should be able to see and change,
 *  rather than "whichever one you touched last". With no prior "Use", copies use `default`. */
function inUse(): ApiTokenRow | null { return pickCopyToken(tokenRows(), rememberedTokenId()); }

/** One token's row (the library row, docs/46 §3.3): its label, "copies use" as a tag on the one
 *  the copy actions embed, the id - a value you copy, so mono - and when it was made; Use (absent
 *  on the token copies already use) and the ⋯ with Rotate and Revoke - red never sits on a row
 *  (design rule 4). The buttons stay delegated on #pane (wire), so the groups component
 *  rebuilding a card never rewires them. */
function rowNode(t: ApiTokenRow): HTMLElement {
  const mine = inUse();
  const used = !!mine && t.id === mine.id;
  const when = t.createdAt ? tr("tokens.createdWhen", { when: new Date(t.createdAt).toLocaleString(locale()) }) : null;
  return row({
    name: used ? [t.label, " ", tag(tr("tokens.copiesUse"))] : t.label,
    sub: [h("code", null, t.id), when ? " · " + when : null],
    primary: used ? undefined : btn(tr("tokens.use"), { data: { tkuse: t.id } }),
    more: moreBtn(tr("tokens.actionsName", { name: t.label }), { data: { tkmore: t.id } }),
    data: { token: t.id },
  });
}

/** The tokens scope's cfg for mountGroup. No drag contract: creation time is the order, the
 *  order route is a 400 — the grip still reorders GROUPS (that is set_names, allowed), but
 *  nothing can drop a row anywhere. */
function tkCfg(): GroupCfg<ApiTokenRow> {
  return {
    scope: "tokens",
    density: "page",
    names: tokenGroupNames().length ? tokenGroupNames() : ["default"],
    collapsed: collapsed,
    noun: tr("tokens.token"),
    addTitle: (g: string): string => { return tr("tokens.createTokenGroup", { group: g }); },
    onAdd: (g: string): void => {
      // The + points at the inline form: pick the group it names and put the cursor in the
      // label box — the sheet scopes open a modal; this page's flow was always inline.
      const sel = $<HTMLSelectElement>("tkGroup");
      if (sel) sel.value = g;
      $<HTMLInputElement>("tkLabel").focus();
    },
    reload: (): Promise<void> => { return refreshTokens().then(paintGroups); },
    render: paintGroups,
    draggable: false,
    rowsById: (): ApiTokenRow[] => { return tokenRows(); },
    groupOfRow: groupOfToken,
    /* rowNode, not rowsHtml + rowSel (docs/37 R5): the component wires the node this
     * builder hands back, so the round trip through a parsed string and a
     * [data-token="..."] lookup - and the CSS.escape dance that lookup needed for ids the
     * server lets be arbitrary - is gone. */
    rowNode: rowNode,
  };
}

/** Keep the form's Group select honest after a group mutation (docs/20 G7): a rename or
 *  delete that the poll reports must not leave a chosen-but-dead option in the box — the
 *  next create would assign into a 400. The selection survives while its group lives. */
function refreshGroupSelect(): void {
  const sel = $<HTMLSelectElement>("tkGroup");
  if (!sel) return;
  const names = tokenGroupNames().length ? tokenGroupNames() : ["default"];
  const wanted = sel.value && names.includes(sel.value)
    ? sel.value
    : resolveDefaultGroup(names, lastGroup("tokens"));
  // Only when the option LIST actually moved. The string version compared rendered markup to
  // decide that; the node version compares the values, which is the thing it meant. Rebuilding
  // options resets the box, and a poll must not do that under a hand that just picked a group.
  const shown = Array.from(sel.options).map((o: HTMLOptionElement): string => { return o.value; });
  const moved = shown.length !== names.length || shown.some((v: string, i: number): boolean => { return v !== names[i]; });
  if (moved) fill(sel, names.map((n: string): HTMLOptionElement => { return h("option", { value: n }, n); }));
  sel.value = wanted;
}

/** (Re)build the groups region only — the create form and the secret box live outside it, so
 *  a repaint never wipes a half-typed label or the one chance to copy a secret. */
function paintGroups(): void {
  const host = $("tkGroups");
  if (!host) return;
  painted = signature();
  const list = tokenRows();
  if (!list.length) {
    fill(host, emptyNode({ icon: "key", title: tr("tokens.tokens"), hint: tr("tokens.oneTokenClientCreate") }));
    refreshGroupSelect();
    return;
  }
  const names = tokenGroupNames().length ? tokenGroupNames() : ["default"];
  fill(host, slice(list, names, groupOfToken).map((g: GroupSlice<ApiTokenRow>): HTMLElement => {
    return mountGroup(tkCfg(), g);
  }));
  refreshGroupSelect();
  $("countChip").textContent = countText();
}

/** The Group select of the create form: the scope's groups, the last-used one selected.
 *  `select.value = picked` rather than a selected attribute on one option - the property is
 *  the selection, the attribute was only ever its initial default. */
function groupSelectNode(): HTMLSelectElement {
  const names = tokenGroupNames().length ? tokenGroupNames() : ["default"];
  const sel = h("select", { id: "tkGroup", title: tr("tokens.groupTokenListsUnder") },
    names.map((n: string): HTMLOptionElement => { return h("option", { value: n }, n); }));
  sel.value = resolveDefaultGroup(names, lastGroup("tokens"));
  return sel;
}

/** The once-only secret box: shown after a create or rotate, kept in state so a poll or the
 *  explicit refresh cannot lose the one chance to copy it. Null when there is nothing to
 *  show - h() drops a null child, so the caller needs no "" branch. */
function secretNode(): HTMLElement | null {
  const secret = tokensDomain.viewSecret;
  if (!secret) return null;
  // Right under the form that made it: the one place the secret is ever shown.
  return section({}, card(form(
    field({
      label: tr("tokens.newSecretCopyNow"),
      control: h("input", { id: "tkSecret", value: secret, readOnly: true, aria: { label: tr("tokens.newTokenSecretShown") } }),
    }),
    formActions(
      btn(tr("tokens.copyConnectCommandsAll"), { kind: "primary", id: "tkCopyConn" }),
      btn(tr("tokens.copySecret"), { id: "tkCopySecret" })))));
}

function render(): void {
  painted = signature();
  // No location title: the context bar already says "MCP / Token". One sentence (U11); which
  // token the copies use is the row's own tag now, not a paragraph about it. New group is the
  // folder-plus glyph - the sidebar's own, for the same act (rule 7).
  fill($("pane"), paneBody({ wide: true },
    paneHead({
      desc: tr("tokens.descOneLine"),
      actions: [iconBtn("folder-plus", tr("tokens.newGroup"), { id: "tkNewGroup" })],
    }),
    // The inline create form (docs/35 §3): one row, the Group select beside the primary.
    inlineForm(
      h("input", { id: "tkLabel", placeholder: tr("tokens.labelEGClaude") }),
      groupSelectNode(),
      btn(tr("tokens.create"), { kind: "primary", id: "tkCreate" })),
    secretNode(),
    section({}, h("div", { id: "tkGroups" }))));
  paintGroups();
  $("countChip").textContent = countText();
  // Re-claimed, not re-attached: fill() keeps #pane itself, so this handler already survives
  // a repaint. What it does not survive is another view taking #pane over - remote,
  // remote-runs and secrets each assign their own - so the claim goes with every render.
  wire();
}

/** Poll-safe update: the groups region only, and only when the structure changed. The
 *  create form and the secret box are never touched from here. */
function patch(): void {
  if (signature() === painted) return;
  paintGroups(); // repaints the rows, the Group select's options and the chip
}

/** Rotate: a new secret at once (the old one stops working), shown in the once-only box. */
async function rotateToken(id: string): Promise<void> {
  const r = await apiJson<ApiTokenCreated>("/api/tokens/" + encodeURIComponent(id) + "/rotate", { method: "POST" });
  if (!r) return;
  await refreshTokens();
  tokensDomain.viewSecret = r.secret;
  if (rememberedTokenId() === r.id) useToken(r.id, r.secret);
  render();
}

/** Revoke after an explicit confirm; a revoked in-use token also clears the copy choice. */
async function revokeToken(id: string): Promise<void> {
  if (!confirm(tr("tokens.revokeTokenClientsUsing"))) return;
  const d = await apiJson<unknown>("/api/tokens/" + encodeURIComponent(id), { method: "DELETE" });
  if (!d) return;
  if (rememberedTokenId() === id) {
    setActiveTokenSecret(null);
    try { localStorage.removeItem(TOKEN_ID_KEY); } catch (e) { /* blocked */ }
  }
  await refreshTokens();
  render();
}

function wire(): void {
  $("pane").onclick = async (event: MouseEvent): Promise<void> => {
    const button = targetEl(event)?.closest<HTMLElement>("[data-tkuse],[data-tkmore],#tkCreate,#tkCopySecret,#tkCopyConn,#tkNewGroup");
    if (!button) return;
    if (button.id === "tkCreate") {
      const j = await apiJson<ApiTokenCreated>("/api/tokens", { method: "POST", body: JSON.stringify({ label: $<HTMLInputElement>("tkLabel").value }) });
      if (!j) return;
      // The selected group follows the new id in one family write; an older gateway without
      // the family keeps the first-group default (the select's only option).
      const picked = $("tkGroup") ? $<HTMLSelectElement>("tkGroup").value : null;
      if (picked && j.id) {
        rememberGroup("tokens", picked);
        await assignMember("tokens", j.id, picked);
      }
      await refreshTokens();
      tokensDomain.viewSecret = j.secret;
      render();
      return;
    }
    if (button.id === "tkNewGroup") {
      newGroupFlow("tokens", tokenGroupNames().length ? tokenGroupNames() : ["default"], (): Promise<void> => {
        return refreshTokens().then((): void => { render(); });
      });
      return;
    }
    if (button.id === "tkCopySecret") { void copyText(tokensDomain.viewSecret!, tr("tokens.tokenSecret")); return; }
    if (button.id === "tkCopyConn") { void copyText(connectAll(tokensDomain.viewSecret!), tr("tokens.connectCommands")); return; }
    if (button.dataset.tkuse) {
      if (await fetchSecret(button.dataset.tkuse)) render();
      return;
    }
    if (button.dataset.tkmore) {
      // The opening click must not reach document (menu.js closes on outside clicks).
      event.stopPropagation();
      const id = button.dataset.tkmore;
      popupMenu(button.getBoundingClientRect(), [
        { label: tr("tokens.rotateSecret"), fn: (): void => { void rotateToken(id!); } },
        { sep: true },
        { label: tr("tokens.revoke"), danger: true, fn: (): void => { void revokeToken(id!); } },
      ]);
    }
  };
}

export async function mount(): Promise<void> {
  tokensDomain.viewSecret = null; // the once-only box is per-visit, like the sheet it replaced
  collapsed = loadCollapsed("tokens"); // before the first paint, so folded groups never flash open
  await refreshTokens();
  render();
}
export async function refresh(): Promise<void> { await refreshTokens(); render(); }
export async function poll(): Promise<void> { await refreshTokens(); patch(); }
export function countText(): string {
  const n = tokenRows().length;
  return trn(n, "tokens.nTokens.one", "tokens.nTokens.other");
}
export function unmount(): void { painted = ""; }

export { pickCopyToken, refreshTokens, rememberedTokenId };