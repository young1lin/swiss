import { $ } from "./util.js";

/* Fullscreen (the toolbar's expand control): the sub-page takes over the whole page area —
 * ONLY the global toolbar folds away (body.immersive, base.css); everything below it is the
 * page's own and stays: the page bar (its header), the sidebar (on #mcps the sidebar IS the
 * page — the MCP list itself), and the content. This is deliberately NOT the browser's F11:
 * the panel never requests document fullscreen — taking over the screen is the user's own
 * keypress; taking over the page is ours. Views never learn the mode exists: the toggle ends
 * by dispatching a window resize, the event every view already answers — the terminal refits
 * its rows/cols and tells the gateway (docs/14 §8), the wide lists re-measure. Exit is the
 * same control, escaped to a corner while fullscreen (base.css), or Esc — the last layer of
 * the panel's Esc chain (main.js). */
var IMMERSIVE = "immersive";

function immersiveOn() { return document.body.classList.contains(IMMERSIVE); }

function paint() {
  var btn = $("expandBtn");
  var on = immersiveOn();
  btn.title = on ? "Exit fullscreen (Esc)" : "Fullscreen — the page takes over the window; F11 stays yours (Esc exits)";
  btn.setAttribute("aria-label", on ? "Exit fullscreen" : "Fullscreen");
  btn.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-' + (on ? "collapse" : "expand") + '"></use></svg>';
}

function toggleImmersive() {
  var on = !immersiveOn();
  document.body.classList.toggle(IMMERSIVE, on);
  paint();
  window.dispatchEvent(new Event("resize"));
}

function exitImmersive() { if (immersiveOn()) toggleImmersive(); }

function initImmersive() { $("expandBtn").onclick = toggleImmersive; paint(); }

export { exitImmersive, immersiveOn, initImmersive, toggleImmersive };
