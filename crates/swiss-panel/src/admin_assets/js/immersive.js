import { $ } from "./util.js";

/* Immersive mode: one control buys the page the top bar's room. Only the global toolbar
 * folds away (body.immersive, base.css) — the page bar, the sidebar and the content are the
 * page's own and stay; entering also asks the browser for real fullscreen — the ask rides
 * the same user gesture, and a browser that refuses it (embedded, blocked) still gets the
 * layout, so the two are ONE gesture with a best-effort bonus, not two modes to explain.
 * Views never learn this mode exists: the toggle ends by dispatching a window resize, the
 * event every view already answers — the terminal refits its rows/cols and tells the gateway
 * (docs/14 §8), the wide lists re-measure. Exit is the same control, escaped to a corner
 * while immersive (base.css), or Esc — the last layer of the panel's Esc chain (main.js). */
var IMMERSIVE = "immersive";

function immersiveOn() { return document.body.classList.contains(IMMERSIVE); }

function paint() {
  var btn = $("expandBtn");
  var on = immersiveOn();
  btn.title = on ? "Exit fullscreen (Esc)" : "Fullscreen — hides the top bar, the page keeps its own (Esc exits)";
  btn.setAttribute("aria-label", on ? "Exit fullscreen" : "Fullscreen");
  btn.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-' + (on ? "collapse" : "expand") + '"></use></svg>';
}

function toggleImmersive() {
  var on = !immersiveOn();
  document.body.classList.toggle(IMMERSIVE, on);
  if (on) {
    var root = document.documentElement;
    if (root.requestFullscreen) root.requestFullscreen().catch(function () { /* refused — immersive alone is still the point */ });
  } else if (document.fullscreenElement && document.exitFullscreen) {
    document.exitFullscreen().catch(function () { /* already leaving */ });
  }
  paint();
  window.dispatchEvent(new Event("resize"));
}

function exitImmersive() { if (immersiveOn()) toggleImmersive(); }

function initImmersive() { $("expandBtn").onclick = toggleImmersive; paint(); }

export { exitImmersive, immersiveOn, initImmersive, toggleImmersive };
