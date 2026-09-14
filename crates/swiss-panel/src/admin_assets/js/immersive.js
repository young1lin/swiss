import { $ } from "./util.js";

/* Fullscreen (the rail's expand control): the page's own workspace takes over the whole
 * page area — the APP chrome folds away (body.immersive, base.css): the plugin rail and,
 * when drawn, the context bar. What stays is the page's own: the resource sidebar on #mcps
 * (the sidebar IS the page — the MCP list itself), the terminal's own toolbar and session
 * tabs on #terminal (a workspace plugin — its chrome is the page), and the content. This is
 * deliberately NOT the browser's F11: the panel never requests document fullscreen — taking
 * over the screen is the user's own keypress; taking over the page is ours. Views never
 * learn the mode exists beyond one optional .imm-toggle button (the terminal's own ⛶): the
 * toggle ends by dispatching a window resize, the event every view already answers — the
 * terminal refits its rows/cols and tells the gateway (docs/14 §8), the wide lists
 * re-measure. Exit is the same control, escaped to a corner while fullscreen (base.css), or
 * Esc — the last layer of the panel's Esc chain (main.js). */
var IMMERSIVE = "immersive";

function immersiveOn() { return document.body.classList.contains(IMMERSIVE); }

/** Repaint every fullscreen affordance: the rail's #expandBtn plus any .imm-toggle a page
 *  mounted (the terminal's own toolbar button), so entering a workspace page while already
 *  fullscreen still shows an honest icon. Guarded for the vitest micro-DOMs. */
function paintImmersive() {
  var on = immersiveOn();
  var btns = [];
  var global = $("expandBtn");
  if (global) btns.push(global);
  if (typeof document.querySelectorAll === "function") {
    Array.prototype.forEach.call(document.querySelectorAll(".imm-toggle"), function (b) { btns.push(b); });
  }
  btns.forEach(function (btn) {
    if (!btn) return;
    btn.title = on ? "Exit fullscreen (Esc)" : "Fullscreen — the page takes over the window; F11 stays yours (Esc exits)";
    if (btn.setAttribute) btn.setAttribute("aria-label", on ? "Exit fullscreen" : "Fullscreen");
    btn.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-' + (on ? "collapse" : "expand") + '"></use></svg>';
  });
}

function toggleImmersive() {
  var on = !immersiveOn();
  document.body.classList.toggle(IMMERSIVE, on);
  paintImmersive();
  window.dispatchEvent(new Event("resize"));
}

function exitImmersive() { if (immersiveOn()) toggleImmersive(); }

function initImmersive() { $("expandBtn").onclick = toggleImmersive; paintImmersive(); }

export { exitImmersive, immersiveOn, initImmersive, paintImmersive, toggleImmersive };
