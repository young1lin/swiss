/* An in-terminal toast: one small pill, centered over the terminal surface, faded in
   and out. ttyd's overlay addon (originally hterm's) proved the shape and every call
   site; this is the vanilla-ESM restyling for this panel's language — tokens only, no
   shadow, nothing focusable. It is the feedback channel a terminal needs that never
   touches the PTY stream: resize geometry, the copy scissors, reconnect states. */

export function createOverlay(holder) {
  var el = null;
  var fade = null;
  var hide = null;
  return {
    show: function (text, ms) {
      if (!el) {
        el = document.createElement("div");
        el.className = "term-overlay";
        el.setAttribute("aria-hidden", "true");
      }
      el.textContent = text;
      if (!el.parentNode) holder.appendChild(el);
      /* Forced reflow so a back-to-back show() can fade the same element in again. */
      void el.offsetWidth;
      el.classList.add("on");
      if (hide) clearTimeout(hide);
      if (fade) clearTimeout(fade);
      hide = setTimeout(function () {
        el.classList.remove("on");
        fade = setTimeout(function () {
          if (el && el.parentNode) el.parentNode.removeChild(el);
        }, 220);
      }, ms || 900);
    },
    dispose: function () {
      if (hide) clearTimeout(hide);
      if (fade) clearTimeout(fade);
      if (el && el.parentNode) el.parentNode.removeChild(el);
      el = null;
    },
  };
}
