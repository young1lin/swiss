import { $ } from "./util.js";

/* Focus mode (the context bar's expand control): the page's body takes over the whole
 * page area — the APP chrome folds away (body.immersive, base.css): the plugin rail and
 * the context bar. What stays is the page's own: the resource sidebar on #mcps (the
 * sidebar IS the page — the MCP list itself), the terminal's own bar and session tabs on
 * #terminal (L3 chrome inside its workspace body), and the content. This is deliberately
 * NOT the browser's F11: the panel never requests document fullscreen — taking over the
 * screen is the user's own keypress; taking over the page is ours. There is exactly ONE
 * shell-owned control: #expandBtn, at the context bar's far right in normal mode. While
 * immersive the bar folds through visibility, and the same button escapes to the
 * upper-right corner (base.css) — entering and leaving Focus share one spatial anchor, so
 * a page never mounts a fullscreen button of its own. The toggle ends by dispatching a
 * window resize, the event every view already answers — the terminal refits its rows/cols
 * and tells the gateway (docs/14 §8), the wide lists re-measure. Esc exits too — the last
 * layer of the panel's Esc chain (main.js). */
var IMMERSIVE = "immersive";

function immersiveOn() { return document.body.classList.contains(IMMERSIVE); }

/** Repaint the shell-owned focus control (icon, title, aria label) for the current mode.
 *  Guarded for the vitest micro-DOMs. */
function paintImmersive() {
  var on = immersiveOn();
  var btn = $("expandBtn");
  if (!btn) return;
  btn.title = on ? "Exit fullscreen (Esc)" : "Fullscreen — the page takes over the window; F11 stays yours (Esc exits)";
  if (btn.setAttribute) btn.setAttribute("aria-label", on ? "Exit fullscreen" : "Fullscreen");
  btn.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-' + (on ? "collapse" : "expand") + '"></use></svg>';
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
