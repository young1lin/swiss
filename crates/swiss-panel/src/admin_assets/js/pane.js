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

                                                
                                                    
                                                  
import { $, KINDS, emptyNode, isMcpKind, targetEl } from "./util.js";
import { fill, h } from "./h.js";
import { openGroupSheet, openSheet } from "./add-sheet.js";
import { copyConn, copyText, endpointUrl, tabBody } from "./connect.js";
import { act, authorizeMcp, removeMcp, renameMcp, showTab, startEdit } from "./detail.js";
import { afterTabPaint, paneTabChange, paneTabClick, paneTabInput, paneTabKeydown } from "./run-history.js";
import { assignGroup, groupOf, rowOf, saveGroups } from "./sidebar.js";
import { currentView } from "./ui-state.js";
import { lastActionOf, mcpBusyVerb, mcpDetail, mcpGroups, mcpRows } from "./mcp-state.js";
import { locale, tk, tr } from "./i18n.js";
import { anchoredMenu, btn, closeMenu, dot, menuOpen, moreBtn, note, paneBody, resHead, seg } from "./ui/index.js";
                                                        

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
  const mark = document.querySelector             ("#pane .pane-sub .dot");
  const txt = document.querySelector             ("#pane .pane-sub .sub-text");
  const primary = $                   ("primaryBtn");
  if (!m || !mark || !txt) return;
  const busyVerb = mcpBusyVerb(d.name);
  if (typeof mark.replaceWith === "function") mark.replaceWith(stateDot(m, busyVerb));
  txt.textContent = headSubtitle(m);
  if (primary) {
    const started = m.lifecycle === "started";
    primary.textContent = busyVerb ? "…" : (started ? tr("pane.disable") : tr("pane.enable"));
    primary.disabled = !!busyVerb;
    // No onclick here (SPEC §panel.toolchain): #pane's delegated click derives the verb from live state.
  }
}

/** The head's state mark: the row's state, or "starting" while a verb this panel sent is in
 *  flight. Its title is the state word the subtitle leads with. */
function stateDot(m                           , busyVerb                           )              {
  const state = (busyVerb ? "starting" : m.state)            ;
  return dot(state, busyVerb ? busyVerb + "…" : m.state === "stopped" ? tr("pane.disabled") : m.state);
}

function headSubtitle(m                           )         {
  const bits = [];
  if (mcpBusyVerb(m.name)) bits.push(mcpBusyVerb(m.name) + "…");
  else bits.push(m.state === "stopped" ? tr("pane.disabled") : m.state); // SPEC §mcp.revisions: the honest word
  bits.push(m.type);
  bits.push(m.source);
  if (m.state === "up" && m.latencyMs != null) bits.push(m.latencyMs + " ms");
  if (m.startedAt) bits.push(tr("pane.sinceTime", { time: new Date(m.startedAt).toLocaleTimeString(locale()) }));
  // The OAuth badge (SPEC §mcp.oauth): stored credentials read authorized; expiry is the
  // gateway's to handle with a refresh, not the badge's to guess at.
  if (m.oauth) bits.push(tr("pane.oauthState", { state: m.oauth }));
  return bits.join("  ·  ");
}

function renderPane()       {
  // Every non-MCP page owns its pane, including pages contributed by future plugins.
  if (currentView() !== "mcps") return;
  const pane = $("pane");
  const d = mcpDetail();
  if (!d) {
    // The shared empty state (SPEC §panel.design), and the one place it carries an action: the pane's
    // own "add" answers the question the empty screen just asked.
    fill(pane, emptyNode(mcpRows().length
      ? { icon: "mcp", title: tr("pane.selectMcp"), hint: tr("pane.toolsResourcesConfigurationAppear") }
      : { icon: "mcp", title: tr("pane.mcpsRegistered"), hint: tr("pane.addOneGroupHeader"), action: tr("pane.addMcp") }));
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
  const menuWasOpen = menuOpen(); // reopened at the end; see the note there
  // The history popover lives on <body>, so a pane rebuild leaves it stranded over whatever tab
  // replaced Run. wireTabBody reopens it when the rebuilt pane IS the Run tab; otherwise drop it.
  if (d.tab !== "run" && d.run && d.run.histOpen) { d.run.histOpen = false; const stray = $("r-hist-pop"); if (stray) stray.remove(); }

  // The title is the MCP's identity, so it is set in the sans face. The mount path is a value you
  // copy, so it keeps the monospace one — down in the status line, where it costs nothing.
  //
  // The head is BUILT (SPEC §panel.toolchain): the MCP's name, description and status line are text nodes,
  // so a name with markup in it is a name, not a payload. It is the library's resource head
  // (SPEC §panel.pages): the name row and the tabs pin, the words between them scroll away.
  // Hoisted so the comparison literals stay out of the h() children (i18n gate).
  const oauthKind = !!d.config && (d.config.auth === "oauth" || d.config.type === "figma");
  const actions = [
    // OAuth MCPs get their authorize action in the header (SPEC §mcp.oauth) — it is the one
    // action this MCP cannot live without until it runs, and Reauthorize is the anytime
    // re-consent path after a revoked grant. Disabled while a flow this panel started is
    // still polling.
    oauthKind
      ? btn(tr(m.oauth === "authorized" ? "pane.reauthorize" : "pane.authorize"), { id: "oauthBtn", disabled: !!d.oauthBusy })
      : null,
    // Tinted only for Start: blue is the affirmative action, and a header full of blue Stop
    // buttons on six healthy MCPs says nothing. Disable is a plain button with the same footprint.
    // SPEC §mcp.revisions: the verb is Disable/Enable, not Stop/Start — a stop that survives a boot and
    // refuses every client IS a disable; the mechanism below keeps the stop/start verbs.
    btn(busyVerb ? "…" : started ? tr("pane.disable") : tr("pane.enable"),
      { kind: started ? undefined : "primary", id: "primaryBtn", disabled: !!busyVerb }),
    moreBtn(tr("pane.moreActions"), { id: "menuBtn" }),
  ].filter((b)                         => b != null);

  /* Tab names are module-level data (SPEC §panel.i18n): stored as keys via tk(), painted through
   tr() so a language flip re-renders them with the page. */
  const TAB_LABELS                         = {
    tools: tk("pane.tools"), resources: tk("pane.resources"), prompts: tk("pane.prompts"),
    run: tk("pane.run"), config: tk("pane.config"), logs: tk("pane.logs"),
  };
  const tabs           = [...KINDS, "run", "config", "logs"];
  const nav = seg(tabs.map((t) => {
    const kd = isMcpKind(t) ? d[t] : null;
    return {
      id: t, label: tr(TAB_LABELS[t] || t),
      n: kd && kd.loaded ? (kd.total != null ? kd.total : kd.items.length) : null,
    };
  }), d.tab, { key: "tab", label: tr("pane.sections") });

  const head = resHead({
    title: d.name,
    desc: m.description || null,
    sub: [stateDot(m, busyVerb), h("code", null, tr("pane.mcpPath", { name: d.name })), "·", h("span", { class: "sub-text" }, headSubtitle(m))],
    actions,
    nav,
  });

  const la = lastActionOf(d.name);

  // One frame holds the lot — head, tabs, body, notes — so the measure is applied once and they
  // all share a left edge. The wide one (SPEC §panel.pages): the resource template, and the frame the
  // head's two layers pin in. One width for every tab: changing tabs must not resize the page, so
  // Logs no longer opts into a wider measure of its own — every tab has it.
  fill(pane, paneBody({ wide: true },
    head,
    h("div", { id: "tabbody" }, tabBody(d, m)),
    m.reason ? note(m.reason, { err: true }) : null,
    la ? note(la.at + " · " + la.msg, { err: !!la.err }) : null));

  // Wire up — ONE delegated claim per event type on the pane (SPEC §panel.toolchain), assigned (not
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

/** The pane's own chrome, one click at a time (SPEC §panel.toolchain): header buttons, the tab bar and the
 *  empty state's action. The ... menu's rows answer through the library menu's own handlers
 *  (anchoredMenu), which stop the click before it reaches this listener. Returns true when the
 *  click was chrome and run-history's tab-body dispatch must not see it.
 *
 *  Behavior note (SPEC §panel.toolchain): the primary button and the menu's Enable/Disable verb are
 *  derived from the LIVE row at click time, not from the object renderPane closed over — a
 *  6 s poll that replaced mcpRows() between render and click can no longer fire Stop at a row
 *  that already stopped (patchDetailHead patched the label; the closure kept the old verb). */
function paneChromeClick(ev            )          {
  const t = targetEl(ev);
  if (!t) return false;
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
  else if (a === "stop" || a === "start") void act(d.name, a); // SPEC §mcp.revisions: Disable/Enable beside Restart
  else if (a === "rename") void renameMcp(d.name);
  else if (a === "delete") void removeMcp(d.name);
  else if (a === "edit") { d.tab = "config"; startEdit(); }
  else if (a === "cp-url") void copyText(endpointUrl(d.name), tr("pane.endpointUrl"));
  else if (a === "cp-claude") void copyConn(d.name, "claude");
  else if (a === "cp-codex") void copyConn(d.name, "codex");
  else if (a === "cp-json") void copyConn(d.name, "json");
}

/** The overflow menu is attached and removed on its own, without re-rendering the pane — otherwise
 *  opening it would rebuild (and clear) a config form that was mid-edit. */
function toggleMenu(d           , m                           )       {
  const wasOpen = menuOpen();
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
  anchoredMenu(host, menuItems(d, rowOf(d.name) || m));
}

/** Grouped like a macOS menu: connect, then which group it is in, then manage, then the destructive
 *  one on its own. The group section is a pick list with a tick, not a submenu — a submenu built from
 *  a string is more machinery than four lines of choices are worth. Group names are labels (text),
 *  and each row's action runs through menuAct, which closes the menu first. */
function menuItems(d           , m                           )             {
  const current = groupOf(m);
  const run = (a        ) => { return ()       => { menuAct(d, a); }; };
  // The server's list is complete (default included) and already in sidebar order.
  const picks             = mcpGroups().map((g) => {
    return { label: g, pick: true, on: g === current, fn: () => { void assignGroup(d.name, g); } };
  });
  return [
    { label: tr("pane.connectClient"), heading: true, fn: () => {} },
    { label: tr("pane.copyClaudeCodeCommand"), fn: run("cp-claude") },
    { label: tr("pane.copyCodexCommand"), fn: run("cp-codex") },
    { label: tr("pane.copyMcpJsonEntry"), fn: run("cp-json") },
    { label: tr("pane.copyEndpointUrl"), fn: run("cp-url") },
    { sep: true },
    { label: tr("pane.group"), heading: true, fn: () => {} },
    ...picks,
    { label: tr("pane.newGroup"), fn: run("new-group") },
    { sep: true },
    {
      label: tr(m.lifecycle === "started" ? "pane.disable" : "pane.enable"),
      // The verb is read from the live row at click time, like the primary button's (SPEC §panel.toolchain).
      fn: () => { const live = rowOf(d.name); menuAct(d, (live || m).lifecycle === "started" ? "stop" : "start"); },
    },
    { label: tr("pane.restart"), fn: run("restart") },
    { label: tr("pane.editConfiguration"), fn: run("edit") },
    { label: tr("pane.rename"), fn: run("rename") },
    { sep: true },
    { label: tr("pane.delete"), danger: true, fn: run("delete") },
  ];
}

export { headSubtitle, menuItems, openMenu, paneHasFocus, patchDetailHead, renderPane, toggleMenu };
