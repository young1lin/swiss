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
   ================================================================================================ */
                                                                        
import { $, api, apiJson, emptyNode, targetEl, toast } from "../util.js";
import { fill, h } from "../h.js";
                                      
import { pluginInventory, reloadPluginInventory } from "../page-registry.js";

let busy                          = {}; // plugin id -> true while its own toggle is in flight
let painted = ""; // the structural signature of the drawn list; a change means rebuild
let autostart                                                                 = null; // { enabled, detail, command } from /api/autostart; null = old gateway
let autostartBusy = false; // true while the OS registration write is in flight

function inv()                     { return pluginInventory() || { plugins: [], revision: 0 }                                 ; }
function rows()                 { return inv().plugins || []; }
function signature()         { return rows().map((p              )         => { return p.id; }).join("\n"); }

function dotClass(p              )         {
  if (busy[p.id] || p.state === "starting") return "starting";
  if (p.state === "stopping") return "stopping";
  if (p.state === "failed") return "down";
  return p.enabled && p.state === "active" ? "up" : "idle";
}

/** What the row says it is doing, in the host's own words — never a guess of our own. */
function stateLabel(p              )         {
  if (busy[p.id]) return "working…";
  return p.enabled ? p.state : "disabled";
}
// (docs/18 V4): the state word left the row — the dot carries it; stateLabel is the dot's
// title (docs/18 V6), so the host's own vocabulary explains the colour on hover.

/** The dependency badge (docs/12 W3): the row ALWAYS names what a plugin requires —
 *  "requires connection-catalog" while the host reports the floor met, "needs X (no
 *  provider)" when it does not — so the build's dependency structure is visible at a glance,
 *  not only in the moment something breaks. Plugins that require nothing say nothing, but the
 *  row always carries the (empty) span so poll-patch has its anchor either way.
 *  Exported pure for the suite: no DOM writes, just the row JSON in and badge children out -
 *  a string, a text-plus-span pair, or null (docs/37 R5: h() children, so a capability name
 *  is a text node and cannot close anything). */
export function requiresBadge(p              )         {
  const requires = p.requires || [];
  if (!requires.length) return null;
  if (p.requiresMet === false) {
    return ["· needs " + requires.join(", ") + " ", h("span", { class: "warn" }, "(no provider)")];
  }
  return "· requires " + requires.join(", ");
}

/** One plugins row (docs/18 V4): dot + name + one grey line (id, pages, requirements,
 *  error) — version rides the row title, the state word is the dot's job — and the toggle
 *  is the panel's switch, the control every other row-level on/off uses. Exported pure;
 *  built with h() (docs/37 R5), so there is no esc() left in it: every server string here
 *  is a text node or an attribute value. */
export function rowNode(p              )              {
  const pages = (p.pages || []).join(", ");
  return h("div", { class: "tun-row", data: { plugin: p.id }, title: p.version ? "v" + p.version : undefined },
    h("span", { class: "dot " + dotClass(p), data: { dot: true }, title: stateLabel(p) }),
    h("div", { class: "tun-main" },
      h("div", { class: "tun-name" }, p.label || p.id, !p.enabled && [" ", h("span", { class: "via" }, "· off")]),
      h("div", { class: "tun-sub" },
        h("code", null, p.id),
        " ",
        h("span", { class: "via" }, pages ? "· pages: " + pages : "· no page"),
        h("span", { class: "via", data: { reqs: true } }, requiresBadge(p)),
        h("span", { data: { err: true } }, p.lastError && [" ", h("span", { class: "via" }, "· " + p.lastError)]))),
    h("div", { class: "tun-acts" },
      h("button", {
        class: "sw",
        data: { toggle: true },
        role: "switch",
        aria: { checked: p.enabled ? "true" : "false", label: "Toggle " + (p.label || p.id) },
        disabled: !!busy[p.id],
      })));
}

/** The start-at-sign-in row — the same vocabulary as a plugin row (dot + name + one grey
 *  line + the panel's switch), because it is the same kind of fact: a thing that is on or
 *  off. The grey line says where the OS registration lives, so the operator can check it
 *  outside the panel; the row title carries the exact command the OS would run. Exported
 *  pure; null (an old gateway without the route) renders nothing. */
export function startupRowNode(a                                                                )                     {
  if (!a) return null;
  return h("div", { class: "tun-row", data: { autostart: true }, title: a.command || "" },
    h("span", { class: "dot " + (a.enabled ? "up" : "idle"), title: a.enabled ? "enabled" : "off" }),
    h("div", { class: "tun-main" },
      h("div", { class: "tun-name" }, "Start swiss when you sign in", !a.enabled && [" ", h("span", { class: "via" }, "· off")]),
      h("div", { class: "tun-sub" }, h("span", { class: "via" }, a.detail || ""))),
    h("div", { class: "tun-acts" },
      h("button", {
        class: "sw",
        data: { "autostart-toggle": true },
        role: "switch",
        aria: { checked: a.enabled ? "true" : "false", label: "Toggle start at sign-in" },
      })));
}

function chipText()         {
  const all = rows();
  const on = all.filter((p              )          => { return p.enabled; }).length;
  const failed = all.filter((p              )          => { return p.state === "failed"; }).length;
  return all.length + (all.length === 1 ? " plugin" : " plugins") + " · " + on + " on" +
    (failed ? " · " + failed + " failed" : "");
}

function render()       {
  painted = signature();
  const all = rows();
  const startup = startupRowNode(autostart);
  // No location title: the context bar already says "Settings / Plugins".
  fill($("pane"), h("div", { class: "wide" },
    h("div", { class: "pane-head" },
      h("div", null,
        h("div", { class: "pane-desc" }, "What this build is composed of. Disabling one stops its subsystem and takes its pages and API routes off the air until it is enabled again; definitions, logs and state files are left alone."))),
    startup && [
      h("div", { class: "sec-head" }, h("span", { class: "sec-cap" }, "Startup")),
      h("div", { class: "group" }, startup),
    ],
    h("div", { class: "sec-head" }, h("span", { class: "sec-cap" }, "Installed")),
    all.length
      ? h("div", { class: "group" }, all.map(rowNode))
      : emptyNode({ icon: "power", title: "No plugins", hint: "This gateway reports an empty inventory." }),
    h("div", { class: "tun-foot", data: { foot: true } },
      h("span", { data: { "foot-text": true } }, chipText()),
      h("span", { class: "tun-foot-rev" }, "revision " + inv().revision))));
  wire();
}

/** Poll-safe update: dots, the state word, the error tail and the button. Never structure. */
function patch()       {
  const pane = $("pane");
  if (!pane.querySelector(".group") || signature() !== painted) { render(); return; }
  Array.prototype.forEach.call(pane.querySelectorAll("[data-plugin]"), (row         )       => {
    let p = null                       ;
    rows().forEach((cand              )       => { if (cand.id === row.getAttribute("data-plugin")) p = cand; });
    if (!p) return;
    const dot = row.querySelector("[data-dot]")                      ;
    // Class and title in one pass (docs/18 V6): the poll patches, never rebuilds, so the
    // title must follow the class or it keeps explaining the state before the last change.
    if (dot) { dot.className = "dot " + dotClass(p); dot.title = stateLabel(p); }
    const err = row.querySelector("[data-err]")                      ;
    if (err) fill(err, p.lastError && [" ", h("span", { class: "via" }, "· " + p.lastError)]);
    const reqs = row.querySelector("[data-reqs]")                      ;
    if (reqs) fill(reqs, requiresBadge(p));
    const button = row.querySelector("[data-toggle]")                            ;
    if (button) {
      button.disabled = !!busy[p.id];
      button.setAttribute("aria-checked", p.enabled ? "true" : "false");
    }
    const name = row.querySelector(".tun-name")                      ;
    if (name) fill(name, p.label || p.id, !p.enabled && [" ", h("span", { class: "via" }, "· off")]);
  });
  const foot = pane.querySelector("[data-foot-text]")                      ;
  if (foot) foot.textContent = chipText();
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
 *  404 and leaves autostart null — the section stays hidden, an old binary never grows a
 *  dead control. */
async function loadAutostart()                {
  const r = await api("/api/autostart");
  if (r.ok) autostart = await r.json()                                                           ;
}

/** Flip the OS registration, then redraw from the host's own read-back — never from our
 *  guess of what the click should have done. */
async function toggleAutostart()                {
  if (!autostart || autostartBusy) return;
  autostartBusy = true;
  const j = await apiJson                                                         ("/api/autostart", {
    method: "PUT",
    body: JSON.stringify({ enabled: !autostart.enabled }),
  });
  autostartBusy = false;
  if (!j) return; // apiJson already toasted the refusal
  autostart = j;
  render();
}

/** Enable or disable one plugin, then redraw from the host's answer — never from our guess of
 *  what the click should have done. The revision goes with the request so a stale list is
 *  refused rather than applied. */
async function toggle(id               )                {
  let p = null                       ;
  rows().forEach((cand              )       => { if (cand.id === id) p = cand; });
  const id_ = id ;
  if (!p || busy[id_]) return;
  const action = p.enabled ? "disable" : "enable";
  busy[id_] = true;
  patch();
  const reply = await apiJson                           ("/api/plugins/" + encodeURIComponent(id_) + "/" + action, {
    method: "POST",
    body: JSON.stringify({ revision: inv().revision }),
  });
  delete busy[id_];
  // Reload either way: a refused write means our copy is stale, and a successful one changed
  // the page list the tab strip is drawn from.
  try { await reloadPluginInventory(); }
  catch (error) { /* reloadPluginInventory already toasted; keep the old rows on screen */ }
  render();
  // The enable stood and the START failed: a 200 whose row carries a lastError. The row already
  // shows it, but the click deserves an answer of its own — otherwise pressing Enable on a
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
