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
   Plugins — the host's own management view.

   Every other tab belongs to a plugin; this one belongs to the host, and that is why it is always
   reachable: a disabled plugin cannot serve the page that would switch it back on. An older
   gateway has no plugin host at all — page-registry.js only adds this tab once /api/plugins
   answers, so reaching mount() already means the inventory exists.

   Two writes, both revision-carrying: enable and disable send the revision the list was drawn
   from, so a second panel that changed something in between loses the race with a visible 409
   instead of silently overwriting it. A failed START is not a failed request — the row comes back
   state="failed" with its lastError, and the retry is pressing Enable again.

   A third, smaller section rides along: start-at-sign-in, the one OS-level host setting. It is
   host-owned like this page, so it lives here rather than in a page of its own — and its store
   is the OS (a registry Run value, a LaunchAgent, a systemd user unit), not the gateway, so the
   toggle reads and writes /api/autostart with no revision to race on.

   Drawn from the library (docs/46 P5): row() and sw() for both sections, section() + card()
   for their frames, pageFoot() for the revision.
   ================================================================================================ */
                                                                        
import { $, api, apiJson, refocusIfIdle, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
                                      
import { pluginInventory, reloadPluginInventory } from "../page-registry.js";
import { tr, trn } from "../i18n.js";
import { card, dot, emptyNode, pageFoot, paneBody, paneHead, row, section, sw, tag } from "../ui/index.js";
                                               

let busy                          = {}; // plugin id -> true while its own toggle is in flight
let painted = ""; // the structural signature of the drawn list; a change means rebuild
let autostart                                                                 = null; // { enabled, detail, command } from /api/autostart; null = old gateway
let autostartBusy = false; // true while the OS registration write is in flight

/* The never-loaded fallback carries every field the inventory answers with, so an
   unreachable-empty list is still a truthful ApiPluginsResponse. */
function inv()                     { return pluginInventory() || { plugins: [], revision: 0, pages: [] }; }
function rows()                 { return inv().plugins || []; }
function signature()         { return rows().map((p              )         => { return p.id; }).join("\n"); }

/** The dot a row needs, or null (docs/46 §3.5). The switch already says on or off, so a plugin
 *  whose state agrees with it - on and serving, on and lazily idle, off - draws no dot at all:
 *  six green dots beside six "on" switches said the same thing twice. A dot appears only when
 *  the state is not what the switch promises: work in flight (the amber pulse) or a failed
 *  start (red, with the reason as the row's red line). An unmet requirement is not a dot: it
 *  is the "no provider" warn tag on the sub-line, beside the capability it names - a static
 *  amber dot would read as the pulse of a start. */
function stateDot(p              )                  {
  if (busy[p.id] || p.state === "starting") return "starting";
  if (p.state === "stopping") return "stopping";
  if (p.state === "failed") return "error";
  return null;
}

/** What the dot says it is doing, in the host's own words - never a guess of our own. */
function stateLabel(p              )         {
  if (busy[p.id]) return tr("plugins.working");
  return p.enabled ? p.state : tr("plugins.disabled");
}

/** The dependency badge (docs/12 W3): the row ALWAYS names what a plugin requires -
 *  "requires connection-catalog" while the host reports the floor met, "needs X" with a
 *  "no provider" warn tag when it does not - so the build's dependency structure is visible
 *  at a glance, not only in the moment something breaks. Plugins that require nothing say
 *  nothing (null). Exported pure for the suite: the row JSON in, h() children out, so a
 *  capability name is a text node and cannot close anything (docs/37 R5). The words carry no
 *  separator of their own: the sub-line puts the " · " between its parts (docs/46 P3). */
export function requiresBadge(p              )         {
  const requires = p.requires || [];
  if (!requires.length) return null;
  if (p.requiresMet === false) {
    return [tr("plugins.needsList", { list: requires.join(", ") }), " ", tag(tr("plugins.provider"), { tone: "warn" })];
  }
  return tr("plugins.requiresList", { list: requires.join(", ") });
}

/** "3 pages", with the list itself on hover: the ids are for the curious, the count is what
 *  a row has room for (docs/46 §3.5; "pages: mcps, traffic, tokens" ran every row long). */
function pagesNode(p              )         {
  const pages = p.pages || [];
  return pages.length
    ? h("span", { title: pages.join(", ") }, trn(pages.length, "plugins.nPages.one", "plugins.nPages.other"))
    : tr("plugins.noPage");
}

/** One plugins row: row() with the name, the id in mono and what the plugin serves and needs
 *  on the sub-line, and the panel's switch. The version rides the row title. The state dot
 *  sits after the name, not in the lead column, because most rows have none (stateDot): a
 *  lead on one row in six would push that one name out of line with the rest. A failed
 *  start's reason replaces the sub-line as the row's one red line. Exported pure; every
 *  server string is a text node or an attribute value (docs/37 R5). */
export function rowNode(p              )              {
  const name = p.label || p.id;
  const state = stateDot(p);
  const reqs = requiresBadge(p);
  return row({
    name: state ? [name, " ", dot(state, stateLabel(p))] : name,
    sub: [h("code", null, p.id), " · ", pagesNode(p), reqs ? [" · ", reqs] : null],
    err: p.lastError || undefined,
    toggle: sw(p.enabled, tr("plugins.toggleName", { name }), { data: { toggle: true }, disabled: !!busy[p.id] }),
    data: { plugin: p.id },
    title: p.version ? "v" + p.version : undefined,
  });
}

/** The start-at-sign-in row - the same row as a plugin's, because it is the same kind of
 *  fact: a thing that is on or off, and the switch says which. The sub-line says where the OS
 *  registration lives, so the operator can check it outside the panel; the row title carries
 *  the exact command the OS would run. Exported pure; null (an old gateway without the route)
 *  renders nothing. */
export function startupRowNode(a                                                                )                     {
  if (!a) return null;
  return row({
    name: tr("plugins.startSwissWhenYou"),
    sub: a.detail || null,
    toggle: sw(a.enabled, tr("plugins.toggleStartSign"), { data: { "autostart-toggle": true }, disabled: autostartBusy }),
    data: { autostart: true },
    title: a.command || undefined,
  });
}

function chipText()         {
  const all = rows();
  const on = all.filter((p              )          => { return p.enabled; }).length;
  const failed = all.filter((p              )          => { return p.state === "failed"; }).length;
  return failed
    ? trn(all.length, "plugins.nPluginsBadFailed.one", "plugins.nPluginsBadFailed.other", { on, bad: failed })
    : trn(all.length, "plugins.nPlugins.one", "plugins.nPlugins.other", { on });
}

function revText()         { return tr("plugins.revisionN", { n: inv().revision }); }

function render()       {
  painted = signature();
  const all = rows();
  const startup = startupRowNode(autostart);
  // No location title: the context bar already says "Settings / Plugins", and its chip is
  // the count the foot used to repeat (rule 25) - the foot keeps the revision alone.
  fill($("pane"), paneBody({ wide: true },
    paneHead({ desc: tr("plugins.descOneLine") }),
    startup ? section({ cap: tr("plugins.startup") }, card(startup)) : null,
    section({ cap: tr("plugins.installed") },
      all.length
        ? card(all.map(rowNode))
        : emptyNode({ icon: "power", title: tr("plugins.plugins"), hint: tr("plugins.gatewayReportsEmptyInventory") })),
    pageFoot({ rev: revText() })));
  wire();
}

/** Bring a drawn row up to a freshly built one without replacing it: the text block (name,
 *  dot, sub-line or red line) is swapped only when it reads differently, and the switch is
 *  the SAME node with its state moved - so a poll never steals keyboard focus from the switch
 *  a hand just pressed, and a hover title under the pointer does not flicker every tick. */
function patchRow(node         , fresh             )       {
  const main = node.querySelector(".lrow-main");
  const next = fresh.querySelector(".lrow-main");
  if (main && next && main.outerHTML !== next.outerHTML) main.replaceWith(next);
  if (fresh.title) node.setAttribute("title", fresh.title); else node.removeAttribute("title");
  const was = node.querySelector                   (".sw");
  const now = fresh.querySelector                   (".sw");
  if (was && now) {
    was.disabled = now.disabled;
    was.setAttribute("aria-checked", now.getAttribute("aria-checked") || "false");
    was.setAttribute("aria-label", now.getAttribute("aria-label") || "");
  }
}

/** Poll-safe update: every row's words, dot and switch, the startup row and the revision.
 *  Structure - a plugin appearing or going - is a rebuild. */
function patch()       {
  const pane = $("pane");
  const rev = pane.querySelector(".page-foot-rev");
  if (!rev || signature() !== painted) { render(); return; }
  rev.textContent = revText();
  Array.prototype.forEach.call(pane.querySelectorAll("[data-plugin]"), (node         )       => {
    const p = rows().find((cand              )          => { return cand.id === node.getAttribute("data-plugin"); });
    if (p) patchRow(node, rowNode(p));
  });
  const startup = pane.querySelector("[data-autostart]");
  const fresh = startupRowNode(autostart);
  if (startup && fresh) patchRow(startup, fresh);
}

function wire()       {
  const pane = $("pane");
  pane.onclick = (event            )       => {
    const asButton = targetEl(event)?.closest("[data-autostart-toggle]");
    if (asButton) { void toggleAutostart(); return; }
    const button = targetEl(event)?.closest("[data-toggle]");
    if (!button) return;
    const prow = button.closest("[data-plugin]");
    if (prow) void toggle(prow.getAttribute("data-plugin"));
  };
}

/** Read the OS registration the way the host reads it. A gateway without the route answers
 *  404 and leaves autostart null - the section stays hidden, an old binary never grows a
 *  dead control. */
async function loadAutostart()                {
  const r = await api("/api/autostart");
  if (r.ok) autostart = await r.json()                                                           ;
}

/** Flip the OS registration, then redraw from the host's own read-back - never from our
 *  guess of what the click should have done. The switch is disabled while the write is out
 *  (see util.ts refocusIfIdle). */
async function toggleAutostart()                {
  if (!autostart || autostartBusy) return;
  const pressed = document.activeElement;
  autostartBusy = true;
  patch();
  const j = await apiJson                                                         ("/api/autostart", {
    method: "PUT",
    body: JSON.stringify({ enabled: !autostart.enabled }),
  });
  autostartBusy = false;
  if (j) autostart = j; // else apiJson already toasted the refusal
  patch();
  refocusIfIdle(pressed);
}

/** Enable or disable one plugin, then redraw from the host's answer - never from our guess of
 *  what the click should have done. The revision goes with the request so a stale list is
 *  refused rather than applied. */
async function toggle(id               )                {
  const p = rows().find((cand              )          => { return cand.id === id; });
  if (!p || !id || busy[id]) return;
  const action = p.enabled ? "disable" : "enable";
  const pressed = document.activeElement;
  busy[id] = true;
  patch();
  const reply = await apiJson                           ("/api/plugins/" + encodeURIComponent(id) + "/" + action, {
    method: "POST",
    body: JSON.stringify({ revision: inv().revision }),
  });
  delete busy[id];
  // Reload either way: a refused write means our copy is stale, and a successful one changed
  // the page list the tab strip is drawn from.
  try { await reloadPluginInventory(); }
  catch (error) { /* reloadPluginInventory already toasted; keep the old rows on screen */ }
  patch(); // in place: the pressed switch is the same node, so its focus can come back
  refocusIfIdle(pressed);
  $("countChip").textContent = chipText();
  // The enable stood and the START failed: a 200 whose row carries a lastError. The row already
  // shows it, but the click deserves an answer of its own - otherwise pressing Enable on a
  // plugin that cannot start looks like nothing happened.
  if (reply && reply.plugin && reply.plugin.lastError) toast(reply.plugin.lastError, true);
}

export async function mount()                {
  busy = {};
  try { await reloadPluginInventory(); } catch (error) { /* the toast is enough; draw what we have */ }
  try { await loadAutostart(); } catch (error) { /* no route or no answer: the section stays hidden */ }
  render();
}
export async function refresh()                { await mount(); }
export async function poll()                {
  try { await reloadPluginInventory(); } catch (error) { return; }
  patch();
}
export function countText()         { return chipText(); }
export function unmount() { busy = {}; painted = ""; autostart = null; }
