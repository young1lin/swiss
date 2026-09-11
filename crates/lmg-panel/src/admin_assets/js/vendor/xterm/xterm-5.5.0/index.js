/* Vendored from @xterm/xterm 5.5.0 — lib/xterm.js and css/xterm.css, byte-identical,
   not minified any further than upstream ships them. The UMD bundle assigns
   window.Terminal when evaluated as a classic script; this shim injects that script and
   the stylesheet once, then hands the constructor back as a named export. Upgrading =
   a new versioned directory plus a changed import path; this directory dies in the same
   commit (docs/14 §2, vendoring rules). */
import { loadClassic, unwrapGlobal } from "../load-classic.js";

export async function loadXterm() {
  await loadClassic(new URL("./xterm.js", import.meta.url));
  var link = document.querySelector("link[data-xterm-css]");
  if (!link) {
    link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = new URL("./xterm.css", import.meta.url);
    link.setAttribute("data-xterm-css", "1");
    document.head.appendChild(link);
  }
  /* The core bundle copies each export onto the global (window.Terminal IS the class),
     but unwrapGlobal also accepts the namespace shape the addon bundles use, so the
     core shim does not care which convention a future xterm picks. */
  var cls = unwrapGlobal(window.Terminal, "Terminal");
  if (!cls) {
    throw new Error("xterm.js loaded but window.Terminal carries no Terminal class");
  }
  return cls;
}
