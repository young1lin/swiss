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
import { $, api, apiJson, emptyHtml, esc, toast } from "../util.js";
import { pluginInventory, reloadPluginInventory } from "../page-registry.js";

var busy: Record<string, boolean> = {}; // plugin id -> true while its own toggle is in flight
var painted = ""; // the structural signature of the drawn list; a change means rebuild
var autostart: { enabled: boolean; detail?: string; command?: string } | null = null; // { enabled, detail, command } from /api/autostart; null = old gateway
var autostartBusy = false; // true while the OS registration write is in flight

function inv(): ApiPluginsResponse { return pluginInventory() || { plugins: [], revision: 0 } as unknown as ApiPluginsResponse; }
function rows(): ApiPluginRow[] { return inv().plugins || []; }
function signature(): string { return rows().map(function (p: ApiPluginRow): string { return p.id; }).join("\n"); }

function dotClass(p: ApiPluginRow): string {
  if (busy[p.id] || p.state === "starting") return "starting";
  if (p.state === "stopping") return "stopping";
  if (p.state === "failed") return "down";
  return p.enabled && p.state === "active" ? "up" : "idle";
}

/** What the row says it is doing, in the host's own words — never a guess of our own. */
function stateLabel(p: ApiPluginRow): string {
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
 *  Exported pure for the suite: no DOM, just the row JSON in and badge HTML out. */
export function requiresBadge(p: ApiPluginRow): string {
  var requires = p.requires || [];
  if (!requires.length) return "";
  if (p.requiresMet === false) {
    return '· needs ' + esc(requires.join(", ")) + ' <span class="warn">(no provider)</span>';
  }
  return "· requires " + esc(requires.join(", "));
}

/** One plugins row (docs/18 V4): dot + name + one grey line (id, pages, requirements,
 *  error) — version rides the row title, the state word is the dot's job — and the toggle
 *  is the panel's switch, the control every other row-level on/off uses. Exported pure. */
export function rowHtml(p: ApiPluginRow): string {
  var pages = (p.pages || []).join(", ");
  return '<div class="tun-row" data-plugin="' + esc(p.id) + '"' +
      (p.version ? ' title="v' + esc(p.version) + '"' : "") + ">" +
      '<span class="dot ' + esc(dotClass(p)) + '" data-dot title="' + esc(stateLabel(p)) + '"></span>' +
      '<div class="tun-main">' +
        '<div class="tun-name">' + esc(p.label || p.id) +
          (p.enabled ? "" : ' <span class="via">· off</span>') + "</div>" +
        '<div class="tun-sub"><code>' + esc(p.id) + "</code>" +
          (pages ? ' <span class="via">· pages: ' + esc(pages) + "</span>" : ' <span class="via">· no page</span>') +
          '<span class="via" data-reqs>' + requiresBadge(p) + "</span>" +
          '<span data-err>' + (p.lastError ? ' <span class="via">· ' + esc(p.lastError) + "</span>" : "") + "</span>" +
        "</div>" +
      "</div>" +
      '<div class="tun-acts">' +
        '<button class="sw" data-toggle role="switch" aria-checked="' + (p.enabled ? "true" : "false") + '"' +
          ' aria-label="Toggle ' + esc(p.label || p.id) + '"' + (busy[p.id] ? " disabled" : "") + "></button>" +
      "</div>" +
    "</div>";
}

/** The start-at-sign-in row — the same vocabulary as a plugin row (dot + name + one grey
 *  line + the panel's switch), because it is the same kind of fact: a thing that is on or
 *  off. The grey line says where the OS registration lives, so the operator can check it
 *  outside the panel; the row title carries the exact command the OS would run. Exported
 *  pure; null (an old gateway without the route) renders nothing. */
export function startupRowHtml(a: { enabled: boolean; detail?: string; command?: string } | null): string {
  if (!a) return "";
  return '<div class="tun-row" data-autostart title="' + esc(a.command || "") + '">' +
    '<span class="dot ' + (a.enabled ? "up" : "idle") + '" title="' + (a.enabled ? "enabled" : "off") + '"></span>' +
    '<div class="tun-main">' +
      '<div class="tun-name">Start swiss when you sign in' +
        (a.enabled ? "" : ' <span class="via">· off</span>') + "</div>" +
      '<div class="tun-sub"><span class="via">' + esc(a.detail || "") + "</span></div>" +
    "</div>" +
    '<div class="tun-acts">' +
      '<button class="sw" data-autostart-toggle role="switch" aria-checked="' + (a.enabled ? "true" : "false") + '"' +
        ' aria-label="Toggle start at sign-in"></button>' +
    "</div>" +
  "</div>";
}

function chipText(): string {
  var all = rows();
  var on = all.filter(function (p: ApiPluginRow): boolean { return p.enabled; }).length;
  var failed = all.filter(function (p: ApiPluginRow): boolean { return p.state === "failed"; }).length;
  return all.length + (all.length === 1 ? " plugin" : " plugins") + " · " + on + " on" +
    (failed ? " · " + failed + " failed" : "");
}

function render(): void {
  painted = signature();
  var all = rows();
  var body = all.length
    ? '<div class="group">' + all.map(rowHtml).join("") + "</div>"
    : emptyHtml({ icon: "power", title: "No plugins", hint: "This gateway reports an empty inventory." });
  // No location title: the context bar already says "Settings / Plugins".
  $("pane").innerHTML = '<div class="wide">' +
    '<div class="pane-head"><div>' +
      '<div class="pane-desc">What this build is composed of. Disabling one stops its subsystem and takes its pages and API routes off the air until it is enabled again; definitions, logs and state files are left alone.</div>' +
    "</div></div>" +
    (autostart
      ? '<div class="sec-head"><span class="sec-cap">Startup</span></div>' +
        '<div class="group">' + startupRowHtml(autostart) + "</div>"
      : "") +
    '<div class="sec-head"><span class="sec-cap">Installed</span></div>' +
    body +
    '<div class="tun-foot" data-foot><span data-foot-text>' + esc(chipText()) + "</span>" +
      '<span class="tun-foot-rev">revision ' + esc(String(inv().revision)) + "</span></div>" +
  "</div>";
  wire();
}

/** Poll-safe update: dots, the state word, the error tail and the button. Never structure. */
function patch(): void {
  var pane = $("pane");
  if (!pane.querySelector(".group") || signature() !== painted) { render(); return; }
  Array.prototype.forEach.call(pane.querySelectorAll("[data-plugin]"), function (row: Element): void {
    var p = null as ApiPluginRow | null;
    rows().forEach(function (cand: ApiPluginRow): void { if (cand.id === row.getAttribute("data-plugin")) p = cand; });
    if (!p) return;
    var dot = row.querySelector("[data-dot]") as HTMLElement | null;
    // Class and title in one pass (docs/18 V6): the poll patches, never rebuilds, so the
    // title must follow the class or it keeps explaining the state before the last change.
    if (dot) { dot.className = "dot " + dotClass(p); dot.title = stateLabel(p); }
    var err = row.querySelector("[data-err]") as HTMLElement | null;
    if (err) err.innerHTML = p.lastError ? ' <span class="via">· ' + esc(p.lastError) + "</span>" : "";
    var reqs = row.querySelector("[data-reqs]") as HTMLElement | null;
    if (reqs) reqs.innerHTML = requiresBadge(p);
    var button = row.querySelector("[data-toggle]") as HTMLButtonElement | null;
    if (button) {
      button.disabled = !!busy[p.id];
      button.setAttribute("aria-checked", p.enabled ? "true" : "false");
    }
    var name = row.querySelector(".tun-name") as HTMLElement | null;
    if (name) name.innerHTML = esc(p.label || p.id) + (p.enabled ? "" : ' <span class="via">· off</span>');
  });
  var foot = pane.querySelector("[data-foot-text]") as HTMLElement | null;
  if (foot) foot.textContent = chipText();
}

function wire(): void {
  var pane = $("pane");
  pane.onclick = function (event: MouseEvent): void {
    var asButton = event.target!.closest!("[data-autostart-toggle]");
    if (asButton) { void toggleAutostart(); return; }
    var button = event.target!.closest!("[data-toggle]");
    if (!button) return;
    var prow = button.closest("[data-plugin]");
    if (prow) void toggle(prow.getAttribute("data-plugin"));
  };
}

/** Read the OS registration the way the host reads it. A gateway without the route answers
 *  404 and leaves autostart null — the section stays hidden, an old binary never grows a
 *  dead control. */
async function loadAutostart(): Promise<void> {
  var r = await api("/api/autostart");
  if (r.ok) autostart = await r.json() as { enabled: boolean; detail?: string; command?: string };
}

/** Flip the OS registration, then redraw from the host's own read-back — never from our
 *  guess of what the click should have done. */
async function toggleAutostart(): Promise<void> {
  if (!autostart || autostartBusy) return;
  autostartBusy = true;
  var j = await apiJson<{ enabled: boolean; detail?: string; command?: string }>("/api/autostart", {
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
async function toggle(id: string | null): Promise<void> {
  var p = null as ApiPluginRow | null;
  rows().forEach(function (cand: ApiPluginRow): void { if (cand.id === id) p = cand; });
  if (!p || busy[id!]) return;
  var action = p.enabled ? "disable" : "enable";
  busy[id!] = true;
  patch();
  var reply = await apiJson<{ plugin?: ApiPluginRow }>("/api/plugins/" + encodeURIComponent(id!) + "/" + action, {
    method: "POST",
    body: JSON.stringify({ revision: inv().revision }),
  });
  delete busy[id!];
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

export async function mount(): Promise<void> {
  busy = {};
  try { await reloadPluginInventory(); } catch (error) { /* the toast is enough; draw what we have */ }
  try { await loadAutostart(); } catch (error) { /* no route or no answer: the section stays hidden */ }
  render();
}
export async function refresh(): Promise<void> { await mount(); }
export async function poll(): Promise<void> {
  try { await reloadPluginInventory(); } catch (error) { return; }
  patch();
}
export function countText(): string { return chipText(); }
export function unmount() { busy = {}; painted = ""; autostart = null; }
