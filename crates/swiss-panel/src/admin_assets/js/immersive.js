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

import { $ } from "./util.js";

/* Focus mode is a shell behavior with one workspace escape hatch. Ordinary pages gain the
 * plugin rail's width and keep a minimal in-flow app bar, so shell controls can never cover
 * page actions. A workspace may instead declare [data-shell-focus-slot]: Terminal uses it
 * to host the same shell-owned app zone in its own toolbar, letting the context bar collapse
 * completely without duplicating controls or surrendering the obvious exit. The app zone is
 * moved, never cloned, so theme and focus keep one id and one handler. A shallow observer on
 * #pane re-docks it after a page render replaces the workspace root; it does not watch xterm's
 * deep output mutations. This still is not browser F11 — Terminal fills the Swiss window,
 * while browser fullscreen remains the user's own action. Every mode change dispatches resize
 * so xterm refits and the gateway receives the new rows and columns (docs/14 §8). */
var IMMERSIVE = "immersive";
var DOCKED = "immersive-docked";
var appZoneNode                     = null;
var appZoneHome                     = null;
var paneObserver                          = null;

function immersiveOn()          { return document.body.classList.contains(IMMERSIVE); }

function focusSlot()                     {
  var pane = $("pane");
  return pane && pane.querySelector ? pane.querySelector("[data-shell-focus-slot]") : null;
}

function placeAppZone()       {
  appZoneNode = appZoneNode || $("appZone");
  appZoneHome = appZoneHome || $("ctxBar");
  var slot = immersiveOn() ? focusSlot() : null;
  var target = slot || appZoneHome;
  var movable = !!(appZoneNode && target && target.appendChild);
  if (movable && appZoneNode.parentNode !== target) target.appendChild(appZoneNode);
  document.body.classList.toggle(DOCKED, !!(slot && movable));
}

/** Repaint the shell-owned control for Focus or Terminal fullscreen. Guarded for tests. */
function paintImmersive()       {
  var on = immersiveOn();
  var docked = document.body.classList.contains(DOCKED);
  var terminal = !!focusSlot();
  var btn = $("expandBtn");
  if (!btn) return;
  btn.title = on
    ? (docked ? "Exit Terminal fullscreen (Esc)" : "Exit focus mode (Esc)")
    : (terminal ? "Terminal fullscreen — fill the Swiss window (Esc exits)" : "Focus mode — hide app navigation (Esc exits)");
  if (btn.setAttribute) btn.setAttribute("aria-label", on
    ? (docked ? "Exit Terminal fullscreen" : "Exit focus mode")
    : (terminal ? "Terminal fullscreen" : "Focus mode"));
  btn.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-' + (on ? "collapse" : "expand") + '"></use></svg>';
}

function syncLayout()       {
  placeAppZone();
  paintImmersive();
}

function toggleImmersive()       {
  var on = !immersiveOn();
  document.body.classList.toggle(IMMERSIVE, on);
  syncLayout();
  window.dispatchEvent(new Event("resize"));
}

function exitImmersive()       { if (immersiveOn()) toggleImmersive(); }

function initImmersive()       {
  appZoneNode = $("appZone");
  appZoneHome = $("ctxBar");
  $("expandBtn").onclick = toggleImmersive;
  var pane = $("pane");
  if (pane && typeof MutationObserver === "function") {
    paneObserver = new MutationObserver(function () {
      syncLayout();
      if (immersiveOn()) window.dispatchEvent(new Event("resize"));
    });
    paneObserver.observe(pane, { childList: true });
  }
  syncLayout();
}

export { exitImmersive, immersiveOn, initImmersive, paintImmersive, toggleImmersive };
