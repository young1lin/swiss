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

                                                
                                                    
                                                  
import { $, KINDS, emptyNode, iconNode, isMcpKind, targetEl } from "./util.js";
import { fill, h } from "./h.js";
import { openGroupSheet, openSheet } from "./add-sheet.js";
import { copyConn, copyText, endpointUrl, tabBody } from "./connect.js";
import { act, authorizeMcp, removeMcp, renameMcp, showTab, startEdit } from "./detail.js";
import { afterTabPaint, paneTabChange, paneTabClick, paneTabInput, paneTabKeydown } from "./run-history.js";
import { assignGroup, groupOf, rowOf, saveGroups } from "./sidebar.js";
import { currentView, menuIsOpen, setMenuOpen } from "./ui-state.js";
import { lastActionOf, mcpBusyVerb, mcpDetail, mcpGroups, mcpRows } from "./mcp-state.js";
import { locale, tk, tr } from "./i18n.js";

/* --- rendering: detail pane ------------------------------------------------------------------- */
/** True when the user is typing inside the pane; a poll must never re-render over that. */
function paneHasFocus()          {
  const a = document.activeElement;
  return !!a && $("pane").contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName);
}

function patchDetailHead()       {
  const d = mcpDetail();
  if (!d) return;
  const m = rowOf(d.name);
  const dot = document.querySelector             ("#pane .pane-sub .dot");
  const txt = document.querySelector             ("#pane .pane-sub .sub-text");
  const primary = $                   ("primaryBtn");
  if (!m || !dot || !txt) return;
  const busyVerb = mcpBusyVerb(d.name);
  dot.className = "dot " + (busyVerb ? "starting" : m.state);
  txt.textContent = headSubtitle(m);
  if (primary) {
    const started = m.lifecycle === "started";
    primary.textContent = busyVerb ? "…" : (started ? tr("Disable") : tr("Enable"));
    primary.disabled = !!busyVerb;
    // No onclick here (docs/37 R5): #pane's delegated click derives the verb from live state.
  }
}

function headSubtitle(m                           )         {
  const bits = [];
  if (mcpBusyVerb(m.name)) bits.push(mcpBusyVerb(m.name) + "…");
  else bits.push(m.state === "stopped" ? tr("disabled") : m.state); // docs/28 D2: the honest word
  bits.push(m.type);
  bits.push(m.source);
  if (m.state === "up" && m.latencyMs != null) bits.push(m.latencyMs + " ms");
  if (m.startedAt) bits.push(tr("since {time}", { time: new Date(m.startedAt).toLocaleTimeString(locale()) }));
  // The OAuth badge (docs/24 D5): stored credentials read authorized; expiry is the
  // gateway's to handle with a refresh, not the badge's to guess at.
  if (m.oauth) bits.push(tr("oauth: {state}", { state: m.oauth }));
  return bits.join("  ·  ");
}

function renderPane()       {
  // Every non-MCP page owns its pane, including pages contributed by future plugins.
  if (currentView() !== "mcps") return;
  const pane = $("pane");
  const d = mcpDetail();
  if (!d) {
    // The shared empty state (docs/18 V7), and the one place it carries an action: the pane's
    // own "add" answers the question the empty screen just asked.
    fill(pane, emptyNode(mcpRows().length
      ? { icon: "mcp", title: tr("Select an MCP"), hint: tr("Its tools, resources and configuration appear here.") }
      : { icon: "mcp", title: tr("No MCPs registered"), hint: tr("Add one with the + on a group header."), action: tr("Add an MCP") }));
    // The empty pane answers ONLY its own button (master had no pane-level listener here):
    // a detail render earlier in this pane's life left the delegated properties behind, and
    // fill() wipes children, never listeners - without this reset one click on the action ran
    // openSheet twice (direct handler + the [data-empty-action] branch in paneChromeClick).
    pane.onclick = null;
    pane.oninput = null;
    pane.onchange = null;
    pane.onkeydown = null;
    const addBtn = pane.querySelector             ("[data-empty-action]");
    if (addBtn) addBtn.onclick = () => { openSheet(null); };
    return;
  }
  const m                            = rowOf(d.name) || { name: d.name, state: "unknown", type: "?", source: "?", lifecycle: "stopped" };
  const started = m.lifecycle === "started";
  const busyVerb = mcpBusyVerb(d.name);
  const menuWasOpen = menuIsOpen(); // reopened at the end; see the note there
  // The history popover lives on <body>, so a pane rebuild leaves it stranded over whatever tab
  // replaced Run. wireTabBody reopens it when the rebuilt pane IS the Run tab; otherwise drop it.
  if (d.tab !== "run" && d.run && d.run.histOpen) { d.run.histOpen = false; const stray = $("r-hist-pop"); if (stray) stray.remove(); }

  // The title is the MCP's identity, so it is set in the sans face. The mount path is a value you
  // copy, so it keeps the monospace one — down in the status line, where it costs nothing.
  //
  // The head is BUILT (docs/37 R5): the MCP's name, description and status line are text nodes,
  // so a name with markup in it is a name, not a payload.
  const head = h("div", { class: "pane-head" },
    h("div", null,
      h("h1", { class: "pane-title" }, d.name),
      m.description ? h("div", { class: "pane-desc" }, m.description) : null,
      h("div", { class: "pane-sub" },
        h("span", { class: "dot " + (busyVerb ? "starting" : m.state) }),
        h("span", { class: "sub-path" }, "/mcp/" + d.name),
        h("span", { class: "sub-text" }, headSubtitle(m)))),
    h("div", { class: "pane-actions" },
      // OAuth MCPs get their authorize action in the header (docs/24 D5) — it is the one
      // action this MCP cannot live without until it runs, and Reauthorize is the anytime
      // re-consent path after a revoked grant. Disabled while a flow this panel started is
      // still polling.
      d.config && (d.config.auth === "oauth" || d.config.type === "figma")
        ? h("button", { class: "btn", id: "oauthBtn", disabled: !!d.oauthBusy },
            m.oauth === "authorized" ? tr("Reauthorize") : tr("Authorize"))
        : null,
      // Tinted only for Start: blue is the affirmative action, and a header full of blue Stop
      // buttons on six healthy MCPs says nothing. Disable is a plain button with the same footprint.
      // docs/28 D2: the verb is Disable/Enable, not Stop/Start — a stop that survives a boot and
      // refuses every client IS a disable; the mechanism below keeps the stop/start verbs.
      h("button", { class: "btn" + (started ? "" : " primary"), id: "primaryBtn", disabled: !!busyVerb },
        busyVerb ? "…" : started ? tr("Disable") : tr("Enable")),
      h("button", { class: "btn icon", id: "menuBtn", aria: { label: tr("More actions") }, title: tr("More actions") },
        iconNode("ellipsis"))));

  /* Tab names are module-level data (docs/38 L7): stored as keys via tk(), painted through
   tr() so a language flip re-renders them with the page. */
  const TAB_LABELS                         = {
    tools: tk("Tools"), resources: tk("Resources"), prompts: tk("Prompts"),
    run: tk("Run"), config: tk("Config"), logs: tk("Logs"),
  };
  const tabs           = [...KINDS, "run", "config", "logs"];
  const seg = h("div", { class: "seg", role: "tablist" }, tabs.map((t) => {
    const kd = isMcpKind(t) ? d[t] : null;
    const count = kd && kd.loaded
      ? h("span", { class: "seg-n" }, String(kd.total != null ? kd.total : kd.items.length))
      : null;
    return h("button", { role: "tab", data: { tab: t }, aria: { selected: d.tab === t ? "true" : "false" } },
      tr(TAB_LABELS[t] || t), count);
  }));

  const la = lastActionOf(d.name);

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
  fill(pane, h("div", { class: "col" },
    head,
    seg,
    h("div", { id: "tabbody" }, tabBody(d, m)),
    m.reason ? h("div", { class: "note err" }, m.reason) : null,
    la ? h("div", { class: "note" + (la.err ? " err" : "") }, la.at + " · " + la.msg) : null));

  // Wire up — ONE delegated claim per event type on the pane (docs/37 R5), assigned (not
  // addEventListener) so a repaint re-assigns the same property instead of stacking listeners.
  // The chrome dispatch lives here; everything inside #tabbody is answered by run-history's
  // dispatchers, which read live state at event time.
  pane.onclick = (ev            )       => {
    if (paneChromeClick(ev)) return;
    paneTabClick(ev);
  };
  pane.oninput = paneTabInput;
  pane.onchange = paneTabChange;
  pane.onkeydown = paneTabKeydown;
  afterTabPaint(d);
  // A late tools/list response re-renders the pane; without this the ... menu you opened a moment
  // ago would just disappear. The menu is state, so it is restored like any other.
  if (menuWasOpen) openMenu(d, m);
}

/** The pane's own chrome, one click at a time (docs/37 R5): header buttons, the tab bar, the
 *  empty state's action, and the ... menu (wireMenu retired — the menu rides #pane's listener
 *  like everything else). Returns true when the click was chrome and run-history's tab-body
 *  dispatch must not see it.
 *
 *  Behavior note (docs/37 §10.1): the primary button and the menu's Enable/Disable verb are
 *  derived from the LIVE row at click time, not from the object renderPane closed over — a
 *  6 s poll that replaced mcpRows() between render and click can no longer fire Stop at a row
 *  that already stopped (patchDetailHead patched the label; the closure kept the old verb). */
function paneChromeClick(ev            )          {
  const t = targetEl(ev);
  if (!t) return false;
  // The ... menu is inside #pane — its buttons arrive here first.
  const pick = t.closest             (".menu [data-grp]");
  if (pick) {
    // stopPropagation is master's wireMenu semantics, carried into the delegation: without it
    // the click reaches connect.ts's document listener, whose histClose() shuts the Run-history
    // popover master kept open across a menu action.
    ev.stopPropagation();
    const d = mcpDetail();
    if (d) void assignGroup(d.name, String(pick.dataset.grp)); // every name in the menu is a real group now
    return true;
  }
  const mBtn = t.closest             (".menu [data-act]");
  if (mBtn) {
    ev.stopPropagation(); // same wireMenu carry-over as the group items above
    const d = mcpDetail();
    if (d) menuAct(d, String(mBtn.dataset.act));
    return true;
  }
  const d = mcpDetail();
  // Buttons are matched by closest(), not t.id: the button's own glyph (svg/use) is what a real
  // pointer click lands on first, and the icon element does not carry the id.
  if (t.closest("#menuBtn") && d) {
    ev.stopPropagation();
    toggleMenu(d, rowOf(d.name) || { name: d.name, state: "unknown", type: "?", source: "?", lifecycle: "stopped" });
    return true;
  }
  if (t.closest("#primaryBtn") && d) {
    const row = rowOf(d.name);
    const started = row ? row.lifecycle === "started" : false;
    void act(d.name, started ? "stop" : "start");
    return true;
  }
  if (t.closest("#oauthBtn") && d) { void authorizeMcp(d.name); return true; }
  const seg = t.closest             (".seg button");
  if (seg) { showTab(seg.dataset.tab ); return true; }
  const emptyAction = t.closest             ("[data-empty-action]");
  if (emptyAction) { openSheet(null); return true; }
  return false;
}

/** The ... menu's actions. The menu closes first, exactly as wireMenu did, so the action runs on
 *  a pane without the overlay (edit swaps the tab body; new-group opens a sheet over it). */
function menuAct(d           , a        )       {
  closeMenu();
  if (a === "new-group") {
    // Make the group, then put this MCP straight into it — otherwise "New group…" from an MCP's
    // own menu would create an empty group and leave the MCP where it was.
    openGroupSheet(null, async (name        ) => {
      if (!await saveGroups(mcpGroups().concat([name]))) return false;
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
  else if (a === "cp-url") void copyText(endpointUrl(d.name), tr("Endpoint URL"));
  else if (a === "cp-claude") void copyConn(d.name, "claude");
  else if (a === "cp-codex") void copyConn(d.name, "codex");
  else if (a === "cp-json") void copyConn(d.name, "json");
}

/** The overflow menu is attached and removed on its own, without re-rendering the pane — otherwise
 *  opening it would rebuild (and clear) a config form that was mid-edit. */
function toggleMenu(d           , m                           )       {
  const wasOpen = menuIsOpen();
  closeMenu();
  if (wasOpen) return;
  openMenu(d, m);
}
function openMenu(d           , m                           )       {
  const host = document.querySelector             ("#pane .pane-actions");
  if (!host) return;
  // Re-resolve the row from live state instead of trusting the one renderPane closed over. The 6s
  // poll's loadList() REPLACES mcpRows() wholesale, and it only patches the header afterwards — so
  // the captured object is orphaned from that moment on. The menu reads two things off it that
  // change (which group the MCP is in, and whether it is config-sourced), and with a stale object
  // the group tick stayed on whatever it was when the pane was last rendered.
  const live = rowOf(d.name) || m;
  host.append(menuNode(live));
  setMenuOpen(true);
  // No per-menu wiring (docs/37 R5): the menu lives inside #pane, so its buttons answer
  // through paneChromeClick like every other click in the pane.
}
function closeMenu()       {
  const node = $("menu");
  if (node) node.remove();
  document.querySelectorAll(".ctx-menu").forEach((m) => { m.remove(); });
  setMenuOpen(false);
}

/** Grouped like a macOS menu: connect, then which group it is in, then manage, then the destructive
 *  one on its own. The group section is a pick list with a tick, not a submenu — a submenu built from
 *  a string is more machinery than four lines of choices are worth. Built as nodes (docs/37 R5):
 *  group names are text, and the buttons carry data-grp/data-act for #pane's delegated click. */
function menuNode(m                           )              {
  const current = groupOf(m);
  // The server's list is complete (default included) and already in sidebar order.
  const picks = mcpGroups().map((g) => {
    return h("button", { class: "pick" + (g === current ? " on" : ""), data: { grp: g } }, g);
  });
  return h("div", { class: "menu", id: "menu" },
    h("div", { class: "menu-cap" }, tr("Connect a client")),
    h("button", { data: { act: "cp-claude" } }, tr("Copy Claude Code command")),
    h("button", { data: { act: "cp-codex" } }, tr("Copy Codex command")),
    h("button", { data: { act: "cp-json" } }, tr("Copy .mcp.json entry")),
    h("button", { data: { act: "cp-url" } }, tr("Copy endpoint URL")),
    h("hr"),
    h("div", { class: "menu-cap" }, tr("Group")),
    picks,
    h("button", { data: { act: "new-group" } }, tr("New group…")),
    h("hr"),
    h("button", { data: { act: m.lifecycle === "started" ? "stop" : "start" } },
      m.lifecycle === "started" ? tr("Disable") : tr("Enable")),
    h("button", { data: { act: "restart" } }, tr("Restart")),
    h("button", { data: { act: "edit" } }, tr("Edit configuration…")),
    h("button", { data: { act: "rename" } }, tr("Rename…")),
    h("hr"),
    h("button", { class: "danger", data: { act: "delete" } }, tr("Delete")));
}

export { closeMenu, headSubtitle, menuNode, openMenu, paneHasFocus, patchDetailHead, renderPane, toggleMenu };