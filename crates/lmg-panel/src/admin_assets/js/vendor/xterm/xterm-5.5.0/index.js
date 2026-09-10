/* Vendored from @xterm/xterm 5.5.0 — lib/xterm.js and css/xterm.css, byte-identical,
   not minified any further than upstream ships them. The UMD bundle assigns
   window.Terminal when evaluated as a classic script; this shim injects that script and
   the stylesheet once, then hands the constructor back as a named export. Upgrading =
   a new versioned directory plus a changed import path; this directory dies in the same
   commit (docs/14 §2, vendoring rules). */
import { loadClassic } from "../load-classic.js";

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
  if (typeof window.Terminal !== "function") {
    throw new Error("xterm.js loaded but window.Terminal is missing");
  }
  return window.Terminal;
}
