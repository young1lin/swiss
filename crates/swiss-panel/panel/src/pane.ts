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

import type { ApiMcpRow } from "./types/api.js";
import type { PhantomMcpRow } from "./types/dom.js";
import type { McpDetail } from "./types/state.js";
import { $, KINDS, emptyHtml, esc, icon, state } from "./util.js";
import { openGroupSheet, openSheet } from "./add-sheet.js";
import { copyConn, copyText, endpointUrl, tabBody } from "./connect.js";
import { act, authorizeMcp, removeMcp, renameMcp, showTab, startEdit } from "./detail.js";
import { wireTabBody } from "./run-history.js";
import { assignGroup, groupOf, rowOf, saveGroups } from "./sidebar.js";
import { currentView, menuIsOpen, setMenuOpen } from "./ui-state.js";

/* --- rendering: detail pane ------------------------------------------------------------------- */
/** True when the user is typing inside the pane; a poll must never re-render over that. */
function paneHasFocus(): boolean {
  const a = document.activeElement;
  return !!a && $("pane").contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName);
}

function patchDetailHead(): void {
  const d = state.detail;
  if (!d) return;
  const m = rowOf(d.name);
  const dot = document.querySelector<HTMLElement>("#pane .pane-sub .dot");
  const txt = document.querySelector<HTMLElement>("#pane .pane-sub .sub-text");
  const primary = $<HTMLButtonElement>("primaryBtn");
  if (!m || !dot || !txt) return;
  const busyVerb = state.busy[d.name];
  dot.className = "dot " + (busyVerb ? "starting" : m.state);
  txt.textContent = headSubtitle(m);
  if (primary) {
    const started = m.lifecycle === "started";
    primary.textContent = busyVerb ? "…" : (started ? "Disable" : "Enable");
    primary.disabled = !!busyVerb;
    primary.onclick = () => { void act(d?.name, started ? "stop" : "start"); };
  }
}

function headSubtitle(m: ApiMcpRow | PhantomMcpRow): string {
  const bits = [];
  if (state.busy[m.name]) bits.push(state.busy[m.name] + "…");
  else bits.push(m.state === "stopped" ? "disabled" : m.state); // docs/28 D2: the honest word
  bits.push(m.type);
  bits.push(m.source);
  if (m.state === "up" && m.latencyMs != null) bits.push(m.latencyMs + " ms");
  if (m.startedAt) bits.push("since " + new Date(m.startedAt).toLocaleTimeString());
  // The OAuth badge (docs/24 D5): stored credentials read authorized; expiry is the
  // gateway's to handle with a refresh, not the badge's to guess at.
  if (m.oauth) bits.push("oauth: " + m.oauth);
  return bits.join("  ·  ");
}

function renderPane(): void {
  // Every non-MCP page owns its pane, including pages contributed by future plugins.
  if (currentView() !== "mcps") return;
  const pane = $("pane");
  const d = state.detail;
  if (!d) {
    // The shared empty state (docs/18 V7), and the one place it carries an action: the pane's
    // own "add" answers the question the empty screen just asked.
    pane.innerHTML = state.mcps.length
      ? emptyHtml({ icon: "mcp", title: "Select an MCP", hint: "Its tools, resources and configuration appear here." })
      : emptyHtml({ icon: "mcp", title: "No MCPs registered", hint: "Add one with the + on a group header.", action: "Add an MCP" });
    const addBtn = pane.querySelector<HTMLElement>("[data-empty-action]");
    if (addBtn) addBtn.onclick = () => { openSheet(null); };
    return;
  }
  const m: ApiMcpRow | PhantomMcpRow = rowOf(d.name) || { name: d.name, state: "unknown", type: "?", source: "?", lifecycle: "stopped" };
  const started = m.lifecycle === "started";
  const busyVerb = state.busy[d.name];
  const menuWasOpen = menuIsOpen(); // reopened at the end; see the note there
  // The history popover lives on <body>, so a pane rebuild leaves it stranded over whatever tab
  // replaced Run. wireTabBody reopens it when the rebuilt pane IS the Run tab; otherwise drop it.
  if (d.tab !== "run" && d.run && d.run.histOpen) { d.run.histOpen = false; const stray = $("r-hist-pop"); if (stray) stray.remove(); }

  // The title is the MCP's identity, so it is set in the sans face. The mount path is a value you
  // copy, so it keeps the monospace one — down in the status line, where it costs nothing.
  const head =
    '<div class="pane-head">' +
      "<div>" +
        '<h1 class="pane-title">' + esc(d.name) + "</h1>" +
        (m.description ? '<div class="pane-desc">' + esc(m.description) + "</div>" : "") +
        '<div class="pane-sub"><span class="dot ' + esc(busyVerb ? "starting" : m.state) + '"></span>' +
          '<span class="sub-path">/mcp/' + esc(d.name) + "</span>" +
          '<span class="sub-text">' + esc(headSubtitle(m)) + "</span></div>" +
      "</div>" +
      '<div class="pane-actions">' +
        // OAuth MCPs get their authorize action in the header (docs/24 D5) — it is the one
        // action this MCP cannot live without until it runs, and Reauthorize is the anytime
        // re-consent path after a revoked grant. Disabled while a flow this panel started is
        // still polling.
        (d.config && (d.config.auth === "oauth" || d.config.type === "figma")
          ? '<button class="btn" id="oauthBtn"' + (d.oauthBusy ? " disabled" : "") + ">" +
            (m.oauth === "authorized" ? "Reauthorize" : "Authorize") + "</button>"
          : "") +
        // Tinted only for Start: blue is the affirmative action, and a header full of blue Stop
        // buttons on six healthy MCPs says nothing. Disable is a plain button with the same footprint.
        // docs/28 D2: the verb is Disable/Enable, not Stop/Start — a stop that survives a boot and
        // refuses every client IS a disable; the mechanism below keeps the stop/start verbs.
        '<button class="btn' + (started ? "" : " primary") + '" id="primaryBtn"' + (busyVerb ? " disabled" : "") + ">" +
          (busyVerb ? "…" : started ? "Disable" : "Enable") + "</button>" +
        '<button class="btn icon" id="menuBtn" aria-label="More actions" title="More actions">' + icon("ellipsis") + "</button>" +
      "</div>" +
    "</div>";

  const tabs = KINDS.concat(["run", "config", "logs"]);
  const seg = '<div class="seg" role="tablist">' + tabs.map((t) => {
    const d_ = d!;
    const kd = KINDS.indexOf(t) >= 0 ? d_[t as "tools" | "resources" | "prompts"] : null;
    const count = kd && kd.loaded ? '<span class="seg-n">' + (kd.total != null ? kd.total : kd.items.length) + "</span>" : "";
    const label = t.charAt(0).toUpperCase() + t.slice(1);
    return '<button role="tab" data-tab="' + t + '" aria-selected="' + (d_.tab === t ? "true" : "false") + '">' + label + count + "</button>";
  }).join("") + "</div>";

  const reason = m.reason ? '<div class="note err">' + esc(m.reason) + "</div>" : "";
  const la = state.lastAction[d.name];
  const actionNote = la ? '<div class="note' + (la.err ? " err" : "") + '">' + esc(la.at + " · " + la.msg) + "</div>" : "";

  // One column holds the lot — header, tab bar, body, notes — so the measure is applied once and
  // they all share a left edge. Capping each of them individually looked identical until the pane
  // started centring: .seg is inline-flex, and an inline-level box ignores the auto margins that
  // centre a block, so the tab bar stayed at the pane's padding edge ~240px left of the card it
  // labels.
  //
  // One width for every tab, too. Logs used to opt into --measure-wide for its arguments column;
  // once the column is centred that widening moves BOTH edges, so switching Config <-> Logs slid the
  // whole page ~220px sideways and back. Changing tabs must not resize the page. The log rows lose
  // nothing structural at the standard measure — .call-arg is a truncating preview, so it just
  // ellipsises earlier, and the full request and reply are one click away in the expanded row.
  pane.innerHTML = '<div class="col">' + head + seg +
    '<div id="tabbody">' + tabBody(d, m) + "</div>" + reason + actionNote + "</div>";

  // Wire up (no inline handlers — names can contain characters that break string-built onclicks).
  $("primaryBtn").onclick = () => { void act(d?.name, started ? "stop" : "start"); };
  const oauthBtn = $("oauthBtn");
  if (oauthBtn) oauthBtn.onclick = () => { void authorizeMcp(d?.name); };
  $("menuBtn").onclick = (ev) => { ev.stopPropagation(); toggleMenu(d!, m); };
  pane.querySelectorAll<HTMLButtonElement>(".seg button").forEach((b) => {
    b.onclick = () => { showTab(b.dataset.tab!); };
  });
  const d_ = d!;
  wireTabBody(d_);
  // A late tools/list response re-renders the pane; without this the ... menu you opened a moment
  // ago would just disappear. The menu is state, so it is restored like any other.
  if (menuWasOpen) openMenu(d_, m);
}

/** The overflow menu is attached and removed on its own, without re-rendering the pane — otherwise
 *  opening it would rebuild (and clear) a config form that was mid-edit. */
function toggleMenu(d: McpDetail, m: ApiMcpRow | PhantomMcpRow): void {
  const wasOpen = menuIsOpen();
  closeMenu();
  if (wasOpen) return;
  openMenu(d, m);
}
function openMenu(d: McpDetail, m: ApiMcpRow | PhantomMcpRow): void {
  const host = document.querySelector<HTMLElement>("#pane .pane-actions");
  if (!host) return;
  // Re-resolve the row from live state instead of trusting the one renderPane closed over. The 6s
  // poll's loadList() REPLACES state.mcps wholesale, and it only patches the header afterwards — so
  // the captured object is orphaned from that moment on. The menu reads two things off it that
  // change (which group the MCP is in, and whether it is config-sourced), and with a stale object
  // the group tick stayed on whatever it was when the pane was last rendered.
  const live = rowOf(d.name) || m;
  host.insertAdjacentHTML("beforeend", menuHtml(live));
  setMenuOpen(true);
  wireMenu(d);
}
function closeMenu(): void {
  const node = $("menu");
  if (node) node.remove();
  document.querySelectorAll(".ctx-menu").forEach((m) => { m.remove(); });
  setMenuOpen(false);
}

/** Grouped like a macOS menu: connect, then which group it is in, then manage, then the destructive
 *  one on its own. The group section is a pick list with a tick, not a submenu — a submenu built from
 *  a string is more machinery than four lines of choices are worth. */
function menuHtml(m: ApiMcpRow | PhantomMcpRow): string {
  const current = groupOf(m);
  // The server's list is complete (default included) and already in sidebar order.
  const picks = state.groups.map((g) => {
    return '<button class="pick' + (g === current ? " on" : "") + '" data-grp="' + esc(g) + '">' + esc(g) + "</button>";
  }).join("");
  return '<div class="menu" id="menu">' +
    '<div class="menu-cap">Connect a client</div>' +
    '<button data-act="cp-claude">Copy Claude Code command</button>' +
    '<button data-act="cp-codex">Copy Codex command</button>' +
    '<button data-act="cp-json">Copy .mcp.json entry</button>' +
    '<button data-act="cp-url">Copy endpoint URL</button>' +
    "<hr>" +
    '<div class="menu-cap">Group</div>' +
    picks +
    '<button data-act="new-group">New group…</button>' +
    "<hr>" +
    '<button data-act="' + (m.lifecycle === "started" ? "stop" : "start") + '">' +
      (m.lifecycle === "started" ? "Disable" : "Enable") + "</button>" +
    '<button data-act="restart">Restart</button>' +
    '<button data-act="edit">Edit configuration…</button>' +
    '<button data-act="rename">Rename…</button>' +
    '<hr><button class="danger" data-act="delete">Delete</button>' +
    "</div>";
}
function wireMenu(d: McpDetail): void {
  $("menu").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    b.onclick = (ev) => {
      ev.stopPropagation();
      closeMenu();
      if (b.dataset.grp !== undefined) {
        void assignGroup(d.name, b.dataset.grp); // every name in the menu is a real group now
        return;
      }
      const a = b.dataset.act as string;
      if (a === "new-group") {
        // Make the group, then put this MCP straight into it — otherwise "New group…" from an MCP's
        // own menu would create an empty group and leave the MCP where it was.
        openGroupSheet(null, async (name: string) => {
          if (!await saveGroups(state.groups.concat([name]))) return false;
          void assignGroup(d.name, name);
          return true;
        });
        return;
      }
      if (a === "restart") void act(d.name, "restart");
      else if (a === "stop" || a === "start") void act(d.name, a); // docs/28 D3: Disable/Enable beside Restart
      else if (a === "rename") void renameMcp(d.name);
      else if (a === "delete") void removeMcp(d.name);
      else if (a === "edit") { d.tab = "config"; startEdit(); }
      else if (a === "cp-url") void copyText(endpointUrl(d.name), "Endpoint URL");
      else if (a === "cp-claude") void copyConn(d.name, "claude");
      else if (a === "cp-codex") void copyConn(d.name, "codex");
      else if (a === "cp-json") void copyConn(d.name, "json");
    };
  });
}

export { closeMenu, headSubtitle, menuHtml, openMenu, paneHasFocus, patchDetailHead, renderPane, toggleMenu, wireMenu };
